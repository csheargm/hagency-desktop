//! Actual child plus fixed HTTP routes: start-reply loss never regenerates intent,
//! an original stopped process is retained, and new work binds only to v2.
use super::*;
#[derive(Default)]
pub(super) struct State {
    pub(super) command: Option<SharedBudgetAttemptCommand>,
    pub(super) processes: BTreeMap<String, SharedBudgetAttemptProcessReceipt>,
    start_inputs: Vec<Value>,
    ticket_sequence: u64,
    ticket_command: Option<String>,
    tickets: BTreeMap<String, (bool, u64)>,
    lose_start_reply: bool,
    refusals: usize,
    offer: Option<SharedDemandOffer>,
    demand_state: String,
    head: Option<SharedExecutionBudget>,
    demand_inputs: Vec<Value>,
    binding: Option<SharedDemandBinding>,
}
pub(super) fn management(path: &str, body: &Value, s: &mut super::super::State) -> Option<Value> {
    if path.contains("/budget-demands/"){return demand_management(path,body,s);}
    if path.ends_with("/budget-work/budget_demand_work/demand") {
        let d=&mut s.shared.budget_attempts;let o=d.offer.as_ref().unwrap();let c=d.command.as_ref().unwrap();let w=s.shared.work.as_ref().unwrap();
        let binding=SharedDemandBinding{version:1,demand_id:o.demand_id.clone(),demand_digest:digest(o).unwrap(),control_revision:o.control_revision,assignment_id:o.assignment_id.clone(),event_id:o.event_id.clone(),received_at_ms:o.received_at_ms,runtime_command_id:c.command_id.clone(),runtime_command_digest:digest(c).unwrap(),work_id:w.id.clone(),work_digest:digest(w).unwrap()};
        binding.validate_for(o,w).unwrap();d.binding=Some(binding.clone());d.demand_state="bound".into();
        return Some(json!({"demand":{"offer":o,"demandDigest":digest(o).unwrap(),"binding":binding},"observedAtMs":now()}));
    }
    if path.ends_with("/refuse-unissued") {
        s.shared.budget_attempts.refusals+=1;
        return Some(json!({"fixtureStatus":409,"error":"shared_budget_head_unchanged"}));
    }
    let at = now();
    let start = path.ends_with("/budget-attempts/start");
    let replay = s.shared.budget_attempts.command.is_some();
    if start {
        s.shared.budget_attempts.start_inputs.push(body.clone());
        if let Some(first) = s.shared.budget_attempts.start_inputs.first() {
            assert_eq!(first, body);
        }
        if s.shared.budget_attempts.command.is_none() {
            let input: SharedBudgetAttemptStartInput = serde_json::from_value(body.clone()).unwrap();
            let original = s.shared.command.clone().unwrap();
            assert!(s.shared.processes.values().any(|r| r.state == "stopped"));
            s.shared.budget_attempts.command = Some(SharedBudgetAttemptCommand {
        web_search: input.web_search, tool_policy: input.tool_policy,
                version: 3,
                capability: SHARED_BUDGET_CAPABILITY.into(),
                operation: "agent.execution.provision".into(),
                command_id: if input.predecessor.process_attempt==1 {"successor_fixture".into()}else{format!("successor_fixture_{}",input.predecessor.process_attempt+1)},
                assignment_id: original.assignment_id.clone(),
                assignment_generation: 1,
                agent_reservation_id: original.agent_reservation_id.clone(),
                process_attempt: input.predecessor.process_attempt+1,
                original_runtime_digest: digest(&original).unwrap(),
                original_runtime_command: original,
                predecessor: input.predecessor,
                budget_observation: input.budget_observation,
                execution_budget: input.execution_budget,
                start_intent_id: input.start_intent_id,
                issued_at_ms: at,
                expires_at_ms: at + 30_000,
            });
        }
        if std::mem::take(&mut s.shared.budget_attempts.lose_start_reply) {
            return None;
        }
    }
    let c = s.shared.budget_attempts.command.clone().unwrap();
    let h = digest(&c).unwrap();
    if path.ends_with("/result") {
        let ticket = body["token"].as_str()
            .and_then(|token| s.shared.budget_attempts.tickets.get(token))
            .filter(|(_, until)| *until > at).copied();
        let Some(ticket) = ticket else {
            return Some(json!({"fixtureStatus":409,"error":"shared_result_ticket_changed"}));
        };
        let r: SharedBudgetAttemptProcessReceipt =
            serde_json::from_value(body["receipt"].clone()).unwrap();
        r.validate_for(&c).unwrap();
        assert_eq!(body["commandDigest"], h);
        if let Some(old) = s.shared.budget_attempts.processes.get(&r.observation_id) {
            assert_eq!(digest(old).unwrap(), digest(&r).unwrap());
        }
        if r.state == "stopped" && s.shared.reply_pending {
            s.shared.stopped_before_reply = true;
        }
        s.shared
            .budget_attempts
            .processes
            .insert(r.observation_id.clone(), r.clone());
        let activity = if r.state == "qualified" && ticket.0 {
            s.shared.activity_epoch = 1;
            Some(
                json!({"authority":{"runtimeCommandId":c.command_id,"epoch":1,"token":"c".repeat(64)},"commandDigest":h,"processDigest":r.process_digest,"expiresAtMs":at+25000}),
            )
        } else {
            None
        };
        return Some(
            json!({"observationId":r.observation_id,"receiptDigest":digest(&r).unwrap(),"activity":activity,"state":if r.state=="not_started" {"stopped"}else{&r.state},"observedAtMs":at}),
        );
    }
    let history = path.ends_with("/recover") || start && replay;
    if path.ends_with("/recover") {
        assert_eq!(body["commandDigest"], h);
    }
    let selected = body["observationId"].as_str();
    let recorded = s
        .shared
        .budget_attempts
        .processes
        .values()
        .filter(|r| selected.is_none_or(|id| id == r.observation_id))
        .max_by_key(|r| r.ledger_revision)
        .cloned();
    let executable = if history { s.historical_executable } else { !s.shared.cleanup && recorded.is_none() };
    let until = if s.expired_ticket { at - 1 } else if executable { (at + 20000).min(c.expires_at_ms) } else { at + 20000 };
    // This fixture serves one selected command. Execute tickets are bounded per
    // original command, as on the server, rather than across unrelated turns.
    if s.shared.budget_attempts.ticket_command.as_deref() != Some(c.command_id.as_str()) {
        s.shared.budget_attempts.tickets.clear();
        s.shared.budget_attempts.ticket_command = Some(c.command_id.clone());
    }
    s.shared.budget_attempts.ticket_sequence += 1;
    let token = format!("{:064x}", s.shared.budget_attempts.ticket_sequence);
    // Paired schema15 contract: history/poll tickets never replace the
    // original finite execute ticket. The presented ticket controls activity.
    s.shared.budget_attempts.tickets.retain(|_, (_, until)| *until > at);
    assert!(s.shared.budget_attempts.tickets.len() < 64);
    s.shared.budget_attempts.tickets.insert(token.clone(), (executable, until));
    Some(
        json!({"delivery":{"command":c,"commandDigest":h,"token":token,"leaseUntilMs":until,"executionAllowed":executable,"cleanupAllowed":!history&&s.shared.cleanup,"recorded":recorded},"observedAtMs":at}),
    )
}

async fn apply_delta(f:&Fixture,c:&SharedRuntimeCommand){
    use hagency_store::owner_resources::budget::*;
    let scope=f.transport.scope(Permission::ExecuteManagedAgents).await.unwrap();
    let grant=f.state.lock().unwrap().grant.clone().unwrap();
    let enrollment=f.transport.enrollment(&scope,&grant,true).await.unwrap();
    let receipt:ManagedAgentReservationReceipt=serde_json::from_value(f.state.lock().unwrap().agent_receipts[&c.reserve_command.command_id].clone()).unwrap();
    let command=AgentBudgetCommand { owner_funded: false,
        version:1,capability:AGENT_BUDGET_CAPABILITY.into(),operation:"agent.budget.increase".into(),command_id:"budget_transport_delta".into(),top_up_id:"budget_transport_topup".into(),top_up_request_digest:"a".repeat(64),approval_command_id:"budget_approval".into(),approval_digest:"b".repeat(64),approving_administrator_mxid:"@assigned_admin:test".into(),authority_revision:1,assignment_id:c.assignment_id.clone(),assignment_generation:1,reserve_command:c.reserve_command.clone(),reserve_command_digest:c.reserve_command_digest.clone(),reserve_receipt:receipt.clone(),reserve_receipt_digest:digest(&receipt).unwrap(),expected_budget_revision:1,expected_budget_digest:initial_budget_digest(&receipt).unwrap(),expected_ceiling_tokens:c.reserve_command.requested_tokens,add_tokens:200,issued_at_ms:now(),expires_at_ms:c.reserve_command.expires_at_ms,
    };
    let proof=VerifiedAgentBudgetDelivery::new(enrollment,command.clone(),digest(&command).unwrap(),true,false,Instant::now()+Duration::from_secs(20)).unwrap();
    assert_eq!(f.registry.apply_managed_budget(proof).await.unwrap().state,"applied");
}
pub(super) async fn prepared()->(Fixture,SharedExecutionTransport,SharedRuntimeCommand){
    prepared_with_tools(None).await
}
pub(super) async fn prepared_with_tools(tools:Option<bool>)->(Fixture,SharedExecutionTransport,SharedRuntimeCommand){
    let(f,t,c)=attempts::predecessor().await;
    apply_delta(&f,&c).await;
    if let Some(yolo)=tools {
        let db=rusqlite::Connection::open(f._root.path().join("registry/domain.sqlite3")).unwrap();
        db.execute("INSERT INTO owner_shared_tool_policies(child_id,yolo,revision) VALUES(?1,?2,1)",rusqlite::params![c.agent_reservation_id,yolo]).unwrap();
    }
    f.state.lock().unwrap().shared.budget_attempts.lose_start_reply=true;
    assert!(t.start_budget_successor(&f.registry,c.clone()).await.is_err());
    assert_eq!(rows(&f,"owner_shared_budget_start_intents"),1);
    assert_eq!(rows(&f,"owner_shared_successor_attempts"),0);
    assert!(matches!(t.start_budget_successor(&f.registry,c.clone()).await.unwrap(),SharedExecutionProgress::HistoricalEvidenceMissing{..}));
    let mut s=f.state.lock().unwrap();
    assert_eq!(s.shared.budget_attempts.start_inputs.len(),2);
    let a=s.shared.budget_attempts.command.clone().unwrap();
    let w=s.shared.work.as_mut().unwrap();w.id="budget_work_fixture".into();w.execution_id="budget_execution_fixture".into();w.runtime_command_digest=digest(&a).unwrap();
    s.shared.idle_work=false;drop(s);
    (f,t,c)
}
pub(super) async fn wait_stopped(f:&Fixture){
    tokio::time::timeout(Duration::from_secs(15),async{loop{
        if f.state.lock().unwrap().shared.budget_attempts.processes.values().any(|r|r.state=="stopped"){break;}
        tokio::time::sleep(Duration::from_millis(20)).await;
    }}).await.unwrap();
}
#[tokio::test]
#[ignore="requires explicitly built offline supervised probe"]
async fn native_shared_interrupted_call_budget_successor_collects_final_usage(){
  for refused_renewal in [false,true] {
    let(f,t,_)=prepared().await;
    t.poll_budget_successor_fixture(&f.registry,launch(&f,"text-interrupt-usage")).await.unwrap();
    method_seen(&f,"turn/start").await;
    if refused_renewal { refuse_renewal_and_wait_for_usage(&f).await; }
    f.owner.suspend().await.unwrap();
    let db=rusqlite::Connection::open(f._root.path().join("registry/domain.sqlite3")).unwrap();
    let row:(String,u64,u64,bool)=db.query_row("SELECT state,reserved_tokens,known_consumed_tokens,usage_complete FROM owner_managed_calls",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
    assert_eq!(row,("settled".into(),0,27,true));
    let raw:String=db.query_row("SELECT payload FROM owner_shared_work_observations ORDER BY ledger_revision DESC LIMIT 1",[],|r|r.get(0)).unwrap();
    let receipt:SharedWorkReceipt=serde_json::from_str(&raw).unwrap();
    assert_eq!(receipt.state,"failed");
    assert!(receipt.usage_complete);
    assert_eq!(receipt.retained_tokens,Some(27));
    assert_eq!(rows(&f,"owner_managed_budget_deltas"),1);
    let methods=std::fs::read_to_string(f._root.path().join("shared-probe/methods")).unwrap();
    assert_eq!(methods.lines().filter(|m|*m=="turn/start").count(),1);
    assert_eq!(methods.lines().filter(|m|*m=="turn/interrupt").count(),1);
    drop(db);
    f.stop().await;
  }
}
#[tokio::test]
#[ignore="requires explicitly built offline supervised probe"]
async fn native_shared_budget_v3_lost_start_reply_retains_intent_and_one_actual_turn(){
    let(f,t,_)=prepared().await;
    f.state.lock().unwrap().shared.lose_work_result=true;
    assert!(matches!(t.poll_budget_successor_fixture(&f.registry,launch(&f,"text-usage")).await.unwrap(),SharedExecutionProgress::Running{..}));
    wait_stopped(&f).await;
    let (command,receipt)={let s=f.state.lock().unwrap();(s.shared.budget_attempts.command.clone().unwrap(),s.shared.receipts.values().next().unwrap().clone())};
    assert_eq!(receipt.runtime_command_digest,digest(&command).unwrap());assert_eq!(receipt.known_consumed_tokens,200);
    t.recover_budget_successor(&f.registry,command.command_id.clone(),digest(&command).unwrap(),None).await.unwrap();
    let methods=std::fs::read_to_string(f._root.path().join("shared-probe/methods")).unwrap();
    assert_eq!(methods.lines().filter(|s|*s=="turn/start").count(),1);
    assert_eq!(rows(&f,"owner_shared_budget_start_intents"),1);assert_eq!(rows(&f,"owner_shared_successor_attempts"),1);assert_eq!(rows(&f,"owner_managed_calls"),1);
    f.stop().await;
}
#[tokio::test]
#[ignore="requires explicitly built offline supervised probe"]
async fn native_shared_budget_v3_uncertain_work_start_never_calls_provider(){
    let(f,t,_)=prepared().await;f.state.lock().unwrap().shared.lose_start=true;
    assert!(matches!(t.poll_budget_successor_fixture(&f.registry,launch(&f,"text-usage")).await.unwrap(),SharedExecutionProgress::Running{..}));
    wait_stopped(&f).await;
    let methods=std::fs::read_to_string(f._root.path().join("shared-probe/methods")).unwrap();assert!(!methods.lines().any(|s|s=="turn/start"));
    assert_eq!(rows(&f,"owner_managed_calls"),1);assert_eq!(f.state.lock().unwrap().shared.starts,1);
    assert_eq!(f.state.lock().unwrap().shared.budget_attempts.refusals,1);
    let db=rusqlite::Connection::open(f._root.path().join("registry/domain.sqlite3")).unwrap();
    let (reserved,issued,consumed):(u64,Option<u64>,u64)=db.query_row("SELECT c.reserved_tokens,p.issue_revision,c.known_consumed_tokens FROM owner_managed_calls c JOIN owner_managed_call_pins p ON p.call_id=c.id",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    assert_eq!((reserved,issued,consumed),(0,None,0));drop(db);
    f.stop().await;
}
fn demand_status(s:&super::super::State)->Value{
    let d=&s.shared.budget_attempts;let o=d.offer.as_ref().unwrap();
    json!({"offer":o,"demandDigest":digest(o).unwrap(),"state":d.demand_state,"executionBudget":d.head,"originalBlock":null,"runtimeCommandId":d.command.as_ref().map(|c|c.command_id.clone()),"binding":d.binding})
}
fn demand_management(path:&str,body:&Value,s:&mut super::super::State)->Option<Value>{
    if path.ends_with("/poll"){return Some(json!({"demand":demand_status(s),"nextAfter":s.shared.command.as_ref().unwrap().assignment_id,"observedAtMs":now()}))}
    if path.ends_with("/recover"){
        assert_eq!(body["demandDigest"],s.shared.budget_attempts.offer.as_ref().unwrap().digest().unwrap());
        return Some(json!({"demand":demand_status(s),"observedAtMs":now()}));
    }
    if path.ends_with("/start"){
        let input:SharedBudgetDemandStartInput=serde_json::from_value(body.clone()).unwrap();input.validate_for(s.shared.budget_attempts.offer.as_ref().unwrap()).unwrap();
        if let Some(old)=s.shared.budget_attempts.demand_inputs.first(){assert_eq!(old,body)}
        s.shared.budget_attempts.demand_inputs.push(body.clone());s.shared.budget_attempts.demand_state="start_committed".into();
        let reply=management("/budget-attempts/start",&serde_json::to_value(input.start).unwrap(),s);
        let h=digest(s.shared.budget_attempts.command.as_ref().unwrap()).unwrap();s.shared.work.as_mut().unwrap().runtime_command_digest=h;
        return reply;
    }
    panic!("unexpected budget demand test route")
}
#[tokio::test]
#[ignore="requires explicitly built offline supervised probe"]
async fn native_shared_budget_v3_original_demand_lost_response_resume_and_bound_work(){
    let(f,t,c)=attempts::predecessor().await;apply_delta(&f,&c).await;
    let scope=f.transport.scope(Permission::ExecuteManagedAgents).await.unwrap();let grant=f.state.lock().unwrap().grant.clone().unwrap();let e=f.transport.enrollment(&scope,&grant,true).await.unwrap();
    let selected=f.registry.shared_budget_successor_selection(e,c.clone()).await.unwrap();
    {
        let mut s=f.state.lock().unwrap();let at=now();
        let o=SharedDemandOffer{version:1,demand_id:"budget_original_demand".into(),assignment_id:c.assignment_id.clone(),event_id:"$budget_original_event".into(),event_digest:"d".repeat(64),control_revision:1,received_at_ms:at,predecessor_command_id:c.command_id.clone(),predecessor_command_digest:digest(&c).unwrap()};
        let w=s.shared.work.as_mut().unwrap();w.id="budget_demand_work".into();w.event_id=o.event_id.clone();w.created_at_ms=at;
        s.shared.budget_attempts.offer=Some(o);s.shared.budget_attempts.head=Some(selected.execution_budget);s.shared.budget_attempts.demand_state="offered".into();s.shared.budget_attempts.lose_start_reply=true;s.shared.idle_work=false;
    }
    assert!(t.poll_budget_demand(&f.registry,None).await.unwrap().outcome.is_err());
    assert_eq!(rows(&f,"owner_shared_budget_start_intents"),1);assert_eq!(rows(&f,"owner_shared_successor_attempts"),0);
    let resumed=t.resume_next_budget_demand(&f.registry,None).await.unwrap().unwrap();
    assert_eq!(resumed.demand_id,"budget_original_demand");
    assert!(matches!(resumed.outcome.unwrap(),crate::console::native::SharedDemandProgress::Execution{progress:SharedExecutionProgress::HistoricalEvidenceMissing{..},..}));
    // Fresh authenticated process delivery is separate from historical demand recovery.
    assert!(matches!(t.poll_budget_successor_fixture(&f.registry,launch(&f,"text-usage")).await.unwrap(),SharedExecutionProgress::Running{..}));
    wait_stopped(&f).await;
    assert_eq!(rows(&f,"owner_shared_demands"),1);assert_eq!(rows(&f,"owner_shared_demand_decisions"),0);assert_eq!(rows(&f,"owner_shared_budget_demand_decisions"),1);assert_eq!(rows(&f,"owner_shared_demand_bindings"),1);
    {let s=f.state.lock().unwrap();assert_eq!(s.shared.budget_attempts.demand_inputs.len(),2);assert_eq!(s.shared.starts,1);assert_eq!(s.shared.work_writes,1);}
    // Further budget-demand polling is terminal history, never a second launch.
    let again=t.poll_budget_demand_fixture(&f.registry,launch(&f,"text-usage")).await.unwrap().outcome.unwrap();
    assert!(matches!(again,crate::console::native::SharedDemandProgress::Historical{..}));
    let methods=std::fs::read_to_string(f._root.path().join("shared-probe/methods")).unwrap();assert_eq!(methods.lines().filter(|s|*s=="turn/start").count(),1);
    f.stop().await;
}

#[tokio::test]
#[ignore="requires explicitly built offline supervised probe"]
async fn native_shared_budget_v3_unclaimed_cleanup_uploads_not_started_without_model_turn(){
    let(f,t,_)=prepared().await;
    f.state.lock().unwrap().shared.cleanup=true;
    let outcome=t.poll_budget_successor(&f.registry,None).await.unwrap();
    assert!(matches!(outcome,SharedExecutionProgress::Recorded{process_state:Some(ref s),..} if s=="not_started"));
    let c={let s=f.state.lock().unwrap();let receipt=s.shared.budget_attempts.processes.values().next().unwrap();assert_eq!(receipt.state,"not_started");assert!(receipt.process_digest.is_none());s.shared.budget_attempts.command.clone().unwrap()};
    let outcome=t.recover_budget_successor(&f.registry,c.command_id.clone(),digest(&c).unwrap(),None).await.unwrap();
    assert!(matches!(outcome,SharedExecutionProgress::Recorded{process_state:Some(ref s),..} if s=="not_started"));
    assert_eq!(rows(&f,"owner_shared_unstarted_attempts"),1);
    {
        let mut s=f.state.lock().unwrap();s.shared.cleanup=false;
        s.shared.budget_attempts.command=None;s.shared.budget_attempts.processes.clear();
        s.shared.budget_attempts.start_inputs.clear();s.shared.budget_attempts.lose_start_reply=true;
    }
    assert!(t.start_budget_successor(&f.registry,c.original_runtime_command.clone()).await.is_err());
    {
        let s=f.state.lock().unwrap();assert_eq!(s.shared.budget_attempts.start_inputs.len(),1,"new start crosses the HTTP validator after explicit not_started evidence");
        let next=s.shared.budget_attempts.command.as_ref().unwrap();assert_eq!(next.process_attempt,c.process_attempt+1);
        assert_eq!(next.predecessor.command_id,c.command_id);assert!(next.predecessor.process_digest.is_empty());
    }
    assert_eq!(rows(&f,"owner_shared_successor_attempts"),0);assert_eq!(rows(&f,"owner_managed_calls"),0);
    let methods=std::fs::read_to_string(f._root.path().join("shared-probe/methods")).unwrap();assert!(!methods.lines().any(|s|s=="turn/start"));
    f.stop().await;
}

#[tokio::test]
#[ignore="requires explicitly built offline supervised probe"]
async fn native_shared_budget_v3_long_stream_upload_recovery_and_bounded_reply(){
    for mode in ["text-long", "text-overflow", "text-escaped", "text-items"] {
        let(f,t,_)=prepared().await;
        f.state.lock().unwrap().shared.lose_completed_work_result=true;
        assert!(matches!(t.poll_budget_successor_fixture(&f.registry,launch(&f,mode)).await.unwrap(),SharedExecutionProgress::Running{..}));
        wait_stopped(&f).await;
        let (command,receipt)={let s=f.state.lock().unwrap();(s.shared.budget_attempts.command.clone().unwrap(),s.shared.receipts.values().find(|r|r.usage_complete).unwrap_or_else(||panic!("{mode}: receipts {:?}",s.shared.receipts.values().map(|r|(&r.state,r.known_consumed_tokens,r.usage_complete,r.body.as_ref().map(|b|b.len()))).collect::<Vec<_>>())).clone())};
        assert_eq!(receipt.state,"completed","{mode}");
        let body=receipt.body.as_ref().unwrap();
        assert!(body.len()>8192 && body.len()<=32*1024,"{mode}");
        if mode=="text-long" {assert_eq!(body,&("研".repeat(6000)+"Offline answer."));}
        else if mode=="text-items" {assert_eq!(body,&format!("正在调研。\n\n## 调研结果\n\n**完成**\n\n{}","正文\n\n".repeat(1200)));}
        else {assert!(body.ends_with("[答复超过单条消息长度限制，以上内容已截断。]"),"{mode}");}
        assert!(serde_json::to_vec(&receipt).unwrap().len()<=48*1024);
        assert_eq!(receipt.known_consumed_tokens,200);assert_eq!(receipt.retained_tokens,Some(200));
        t.recover_budget_successor(&f.registry,command.command_id.clone(),digest(&command).unwrap(),None).await.unwrap();
        let methods=std::fs::read_to_string(f._root.path().join("shared-probe/methods")).unwrap();
        assert_eq!(methods.lines().filter(|s|*s=="turn/start").count(),1);
        assert_eq!(rows(&f,"owner_managed_calls"),1);
        f.stop().await;
    }
}
