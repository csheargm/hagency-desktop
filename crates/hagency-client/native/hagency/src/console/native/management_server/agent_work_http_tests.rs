use super::*;

fn index(project: Option<&str>, after: Option<&str>, limit: Option<u16>) -> ManagementCommand {
    ManagementCommand::AgentWorkList {
        include_pool_agents: false,
        project_id: project.map(str::to_owned),
        after: after.map(str::to_owned),
        limit,
    }
}
#[tokio::test]
async fn native_agent_work_index_remains_read_only_and_drops_nonpublic_fields() {
    let f = Fixture::new().await;
    let m = manager(
        &f,
        65,
        &[Permission::ReadAgentWork, Permission::ReadIntents],
    )
    .await;
    f.state.lock().unwrap().admission.stage_replies.insert("index".into(),json!({"requests":{"items":[{"requestId":"request_b","projectId":"project_fixture","projectName":"Project","agentName":"Writer","receipt":"private"}],"nextAfter":"request_b"},"observedAtMs":1000}));
    let result = m
        .central(index(Some("project_fixture"), Some("request_a"), Some(1)))
        .await
        .unwrap();
    assert_eq!(result["requests"]["items"][0]["agentName"], "Writer");
    assert!(!result.to_string().contains("private"));
    assert_eq!(
        f.state.lock().unwrap().admission.work_queries[0],
        json!({"projectId":"project_fixture","after":"request_a","limit":1})
    );
    assert!(m.saved_commands().await.unwrap().is_empty());
    assert_eq!(f.state.lock().unwrap().admission.writes, 0);
    assert_eq!(
        m.central(index(Some("different_project"), None, Some(16)))
            .await
            .unwrap_err()
            .code,
        "management_agent_work_invalid"
    );
    assert_eq!(
        m.central(index(None, Some("request_b"), Some(16)))
            .await
            .unwrap_err()
            .code,
        "management_agent_work_invalid"
    );
    f.state.lock().unwrap().admission.invalidate_stage = Some("index".into());
    assert!(m.central(index(None, None, None)).await.is_err());
    m.retire();
    drop(m);
    f.stop().await;
}

#[tokio::test]
async fn native_agent_work_pool_index_requires_opt_in_and_unambiguous_source() {
    let f = Fixture::new().await;
    let m = manager(&f, 66, &[Permission::ReadAgentWork, Permission::ReadIntents]).await;
    let entry = json!({"requestId":"request_pool","poolId":"pool_fixture","poolName":"Resource pool","agentName":"Pool helper","token":"private"});
    let reply = |entry| json!({"requests":{"items":[entry],"nextAfter":null},"observedAtMs":1000});
    f.state.lock().unwrap().admission.stage_replies.insert("index".into(), reply(entry.clone()));
    assert_eq!(m.central(index(None, None, None)).await.unwrap_err().code, "management_agent_work_invalid");
    let command = || ManagementCommand::AgentWorkList {project_id:None,after:None,limit:Some(16),include_pool_agents:true};
    let result = m.central(command()).await.unwrap();
    assert_eq!(result["requests"]["items"][0]["poolName"], "Resource pool");
    assert_eq!(result["requests"]["items"][0]["projectId"], Value::Null);
    assert!(!result.to_string().contains("private"));
    assert_eq!(f.state.lock().unwrap().admission.work_queries.last().unwrap()["includePoolAgents"], true);
    for (field, value) in [("projectId", json!("project")), ("poolName", json!("")), ("poolId", Value::Null)] {
        let mut entry = entry.clone();
        entry[field] = value;
        f.state.lock().unwrap().admission.stage_replies.insert("index".into(), reply(entry));
        assert_eq!(m.central(command()).await.unwrap_err().code, "management_agent_work_invalid");
    }
    assert!(m.saved_commands().await.unwrap().is_empty());
    assert_eq!(f.state.lock().unwrap().admission.writes, 0);
    m.retire(); drop(m); f.stop().await;
}

fn query(after: Option<&str>, limit: Option<u16>) -> ManagementCommand {
    ManagementCommand::AgentWork {
        id: "human_request".into(),
        after: after.map(str::to_owned),
        limit,
    }
}
fn row(id: &str, usage: Value) -> Value {
    json!({"id":id,"executionId":"execution_original","eventId":"$event:test","requesterMxid":"@owner:test","state":"completed","replyState":"sent","createdAtMs":1000,"startedAtMs":2000,"completedAtMs":3000,"usage":usage,"prompt":"private prompt","reply":"private reply","token":"private token"})
}
fn page(items: Vec<Value>, after: Option<&str>) -> Value {
    json!({"work":{"items":items,"nextAfter":after,"receipt":"private receipt"},"observedAtMs":4000,"token":"private token"})
}
fn install(f: &Fixture, value: Value) {
    f.state
        .lock()
        .unwrap()
        .admission
        .stage_replies
        .insert("work/query".into(), value);
}
#[tokio::test]
async fn native_agent_work_preserves_partial_usage_and_exact_paging_without_journal() {
    let f = Fixture::new().await;
    let m = manager(
        &f,
        60,
        &[Permission::ReadAgentWork, Permission::ReadIntents],
    )
    .await;
    let unknown = row("work_a", Value::Null);
    let partial = row(
        "work_b",
        json!({"knownConsumedTokens":17,"retainedTokens":null,"complete":false,"periodKey":"2026-10"}),
    );
    install(&f, page(vec![unknown, partial], Some("work_b")));
    let first = m.central(query(None, Some(2))).await.unwrap();
    assert_eq!(first["requestId"], "human_request");
    assert_eq!(first["work"]["items"][0]["usage"], Value::Null);
    assert_eq!(
        first["work"]["items"][1]["usage"]["retainedTokens"],
        Value::Null
    );
    assert_eq!(
        first["work"]["items"][1]["usage"]["knownConsumedTokens"],
        17
    );
    assert_eq!(first["work"]["items"][1]["usage"]["complete"], false);
    assert!(!first.to_string().contains("private"));
    let complete = row(
        "work_c",
        json!({"knownConsumedTokens":17,"retainedTokens":17,"complete":true,"periodKey":"2026-10"}),
    );
    let zero = row(
        "work_d",
        json!({"knownConsumedTokens":0,"retainedTokens":0,"complete":true,"periodKey":"2026-10"}),
    );
    install(&f, page(vec![complete, zero], None));
    let second = m.central(query(Some("work_b"), Some(2))).await.unwrap();
    assert_eq!(second["work"]["items"][0]["usage"]["retainedTokens"], 17);
    assert_eq!(
        second["work"]["items"][1]["usage"]["knownConsumedTokens"],
        0
    );
    let queries = f.state.lock().unwrap().admission.work_queries.clone();
    assert_eq!(
        queries,
        vec![
            json!({"after":null,"limit":2}),
            json!({"after":"work_b","limit":2})
        ]
    );
    assert_eq!(f.state.lock().unwrap().admission.writes, 0);
    assert!(m.saved_commands().await.unwrap().is_empty());
    drop(m);
    f.stop().await;
}
#[tokio::test]
async fn native_agent_work_requires_its_own_grant_and_current_sdk_after_http() {
    let f = Fixture::new().await;
    let denied = manager(
        &f,
        61,
        &[Permission::ReadAgentStatus, Permission::ReadAgentRequests],
    )
    .await;
    assert_eq!(
        denied.central(query(None, None)).await.unwrap_err().code,
        "management_permission_required"
    );
    assert!(f.state.lock().unwrap().admission.work_queries.is_empty());
    drop(denied);
    let m = manager(&f, 62, &[Permission::ReadAgentWork]).await;
    f.state.lock().unwrap().admission.invalidate_stage = Some("work/query".into());
    assert!(m.central(query(None, None)).await.is_err());
    assert_eq!(f.state.lock().unwrap().admission.work_queries.len(), 1);
    assert_eq!(f.state.lock().unwrap().admission.writes, 0);
    m.retire();
    drop(m);
    f.stop().await;
}
#[tokio::test]
async fn native_agent_work_keeps_policy_auth_transport_errors_distinct_and_empty_is_real() {
    let f = Fixture::new().await;
    let m = manager(&f, 63, &[Permission::ReadAgentWork]).await;
    for (code, status) in [
        ("shared_execution_unavailable", 503),
        ("agent_request_not_found", 404),
        ("owner_authorization_required", 401),
        ("network_unavailable", 503),
    ] {
        install(&f, json!({"code":code,"fixtureStatus":status}));
        let error = m.central(query(None, None)).await.unwrap_err();
        assert_eq!(error.code, code);
    }
    install(&f, page(vec![], None));
    assert_eq!(
        m.central(query(None, None)).await.unwrap()["work"]["items"],
        json!([])
    );
    install(&f, json!({"code":"shared_execution_unavailable","fixtureStatus":401}));
    assert_eq!(m.central(query(None,None)).await.unwrap_err().code,"management_agent_work_invalid");
    drop(m);
    f.stop().await;
}
#[tokio::test]
async fn native_agent_work_rejects_bad_queries_and_unbounded_or_inconsistent_observations() {
    let f = Fixture::new().await;
    let m = manager(&f, 64, &[Permission::ReadAgentWork]).await;
    for command in [
        query(None, Some(0)),
        query(None, Some(17)),
        query(Some("../foreign"), None),
        ManagementCommand::AgentWork {
            id: "../other".into(),
            after: None,
            limit: None,
        },
    ] {
        assert!(m.central(command).await.is_err());
    }
    assert!(f.state.lock().unwrap().admission.work_queries.is_empty());
    let valid = row("work_a", Value::Null);
    let mut negative = valid.clone();
    negative["usage"] = json!({"knownConsumedTokens":-1,"retainedTokens":null,"complete":false,"periodKey":"2026-10"});
    let mut false_complete = valid.clone();
    false_complete["usage"] = json!({"knownConsumedTokens":17,"retainedTokens":null,"complete":true,"periodKey":"2026-10"});
    let mut unsafe_number = valid.clone();
    unsafe_number["createdAtMs"] = json!(9_007_199_254_740_992i64);
    let mut bad_period = valid.clone();
    bad_period["usage"] =
        json!({"knownConsumedTokens":0,"retainedTokens":0,"complete":true,"periodKey":"2026-99"});
    for value in [
        json!({"observedAtMs":4000}),
        page(vec![valid.clone(); 17], None),
        page(vec![valid.clone(), valid.clone()], None),
        page(vec![valid.clone()], Some("other")),
        page(vec![], Some("work_a")),
        page(vec![negative], None),
        page(vec![false_complete], None),
        page(vec![unsafe_number], None),
        page(vec![bad_period], None),
    ] {
        install(&f, value);
        assert_eq!(
            m.central(query(None, None)).await.unwrap_err().code,
            "management_agent_work_invalid"
        );
    }
    install(&f, page(vec![valid], None));
    assert_eq!(
        m.central(query(Some("work_a"), Some(1)))
            .await
            .unwrap_err()
            .code,
        "management_agent_work_invalid"
    );
    assert_eq!(f.state.lock().unwrap().admission.writes, 0);
    drop(m);
    f.stop().await;
}
