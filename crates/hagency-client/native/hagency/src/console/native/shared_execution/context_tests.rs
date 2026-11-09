//! Original offline native processes retain their own sessions. Every new
//! input contains only the current request; receipts remain independently bound.
use super::*;
pub(super) struct State {
    pub active: bool,
    pub v3: bool,
    plain_mode: bool,
    works: BTreeMap<String, SharedWorkItem>,
    contexts: BTreeMap<String, SharedContextRecord>,
    deliveries: BTreeMap<String, SharedDeliveryRecord>,
    starts: usize,
    lose_start: bool,
    wrong_thread: bool,
    rich: bool,
}
impl Default for State {
    fn default() -> Self {
        Self { active: true, v3: true, plain_mode: false, works: BTreeMap::new(), contexts: BTreeMap::new(),
            deliveries: BTreeMap::new(), starts: 0, lose_start: false, wrong_thread: false, rich: false }
    }
}
fn hash_text(v: &str) -> String {
    format!("{:x}", Sha256::digest(v.as_bytes()))
}
pub(super) fn management(
    path: &str,
    body: &Value,
    s: &mut super::super::State,
) -> Option<Option<Value>> {
    if !s.shared.contexts.active
        || !(path.ends_with("/input-mode")
            || path.contains("/context")
            || path.ends_with("/reply/observe"))
    {
        return None;
    }
    let w = s.shared.work.clone().unwrap();
    let wh = digest(&w).unwrap();
    assert_eq!(body["workDigest"], wh);
    let at = now();
    if path.ends_with("/input-mode") {
        s.shared.contexts.works.insert(w.id.clone(), w.clone());
        let c = s.shared.contexts.contexts.get(&w.id);
        return Some(Some(
            json!({"mode":if s.shared.contexts.plain_mode {"plain_text_v1"} else {"thread_context_v1"},"contextDigest":c.map(|c|&c.context_digest),"inputDigest":c.map(|c|&c.input_digest),"observedAtMs":at}),
        ));
    }
    if path.ends_with("/context") || path.ends_with("/context/recover") {
        if !s.shared.contexts.contexts.contains_key(&w.id) {
            assert!(s.shared.contexts.v3);
            assert_eq!(body["version"], 3);
            assert!(body.get("budget").is_none());
            #[cfg(feature = "shared-intake")]
            let binding = demands::context_binding(s);
            #[cfg(not(feature = "shared-intake"))]
            let binding: Option<SharedDemandBinding> = None;
            let c = SharedThreadContext {
                version: 3, input_profile: INPUT_PROFILE_V3.into(),
                work_id: w.id.clone(), work_digest: wh,
                assignment_id: w.assignment_id.clone(),
                runtime_command_digest: w.runtime_command_digest.clone(),
                demand_binding_digest: binding.as_ref().map(|b| digest(b).unwrap()),
                control_revision: binding.map(|b| b.control_revision),
                room_id: w.room_id.clone(), thread_root: w.thread_root.clone(),
                event_id: w.event_id.clone(), received_at_ms: w.created_at_ms,
                context_ordinal: s.shared.contexts.contexts.len() as u64 + 1,
                coverage: COVERAGE_NATIVE_HISTORY.into(), omitted_earlier: true,
                history: vec![],
            };
            let record = SharedContextRecord {
                context_digest: digest(&c).unwrap(),
                input_digest: hash_text(&render_context(&c, &w).unwrap()),
                context: c,
            };
            s.shared.contexts.contexts.insert(w.id.clone(), record);
        }
        let mut c = s.shared.contexts.contexts[&w.id].clone();
        if s.shared.contexts.wrong_thread {
            c.context.thread_root = "$foreign:test".into();
        }
        return Some(Some(
            json!({"context":c.context,"contextDigest":c.context_digest,"inputDigest":c.input_digest,"observedAtMs":at}),
        ));
    }
    if path.ends_with("/context/start") {
        let c = &s.shared.contexts.contexts[&w.id];
        assert_eq!(body["contextDigest"], c.context_digest);
        assert_eq!(body["inputDigest"], c.input_digest);
        s.shared.starts += 1;
        s.shared.contexts.starts += 1;
        let newly_started = s.shared.work_starts.insert(w.id.clone());
        if std::mem::take(&mut s.shared.contexts.lose_start)
            || std::mem::take(&mut s.shared.lose_start)
        {
            return Some(None);
        }
        return Some(Some(
            json!({"start":{"work":w,"workDigest":wh,"activityEpoch":s.shared.activity_epoch,"newlyStarted":!s.shared.false_start && newly_started},"contextDigest":c.context_digest,"inputDigest":c.input_digest,"observedAtMs":at}),
        ));
    }
    let receipt = s
        .shared
        .receipts
        .values()
        .filter(|r| r.work_id == w.id)
        .max_by_key(|r| r.ledger_revision)
        .unwrap();
    let Some(text) = receipt.body.clone() else {
        // A missing native idle/terminal receipt has no outgoing Matrix reply.
        // Preserve unknown accounting without inventing delivery evidence.
        return Some(Some(json!({"state":null,"delivery":null,"observedAtMs":at})));
    };
    let mut content = json!({"msgtype":"m.text","body":text,"m.relates_to":{"rel_type":"m.thread","event_id":w.thread_root,"is_falling_back":true,"m.in_reply_to":{"event_id":w.thread_root}}});
    let formatted_body = s.shared.contexts.rich.then(|| "<p>HTML receipt evidence only</p>".to_string());
    if let Some(body) = &formatted_body {
        content["format"] = json!("org.matrix.custom.html");
        content["formatted_body"] = json!(body);
    }
    let d = SharedReplyDelivery {
        version: 1,
        work_id: w.id.clone(),
        work_digest: wh,
        assignment_id: w.assignment_id,
        runtime_command_digest: w.runtime_command_digest,
        room_id: w.room_id,
        thread_root: w.thread_root,
        source_event_id: w.event_id,
        matrix_txn_id: format!("hagency_shared_{}", w.id),
        matrix_event_id: format!("$reply_{}", w.id),
        matrix_content_digest: digest(&content).unwrap(),
        body: text.clone(),
        formatted_body,
        body_digest: hash_text(&text),
        sent_at_ms: at,
    };
    let record = s
        .shared
        .contexts
        .deliveries
        .entry(w.id)
        .or_insert_with(|| SharedDeliveryRecord {
            receipt_digest: digest(&d).unwrap(),
            receipt: d,
        });
    Some(Some(
        json!({"state":"sent","delivery":record,"observedAtMs":at}),
    ))
}
#[cfg(feature = "shared-intake")]
fn offer(f: &Fixture, n: u64, thread: &str, body: &str) {
    demands::offer(f, n);
    let mut s = f.state.lock().unwrap();
    s.shared.contexts.active = true;
    let w = s.shared.work.as_mut().unwrap();
    w.thread_root = thread.into();
    w.body = body.into();
    w.body_digest = hash_text(body);
}
#[tokio::test]
#[cfg(feature = "shared-intake")]
#[ignore = "requires explicit offline context probe"]
async fn native_shared_context_room_workspaces_follow_actual_work_and_survive_successor_processes() {
    let (f, t, command) = demands::fixture_budget(2000).await;
    let mut expected = Vec::new();
    let room = command.reserve_command.room_id.as_str();
    for (n, thread) in [(1, "$one"), (2, "$two"), (3, "$three")] {
        offer(&f, n, thread, "Offline workspace request");
        let prepared = launch(&f, "text-context-workspace");
        let root = f._root.path().join("shared-probe").canonicalize().unwrap();
        expected.push(root.join("workspaces").join(&command.reserve_command.proposed_agent_id)
            .join("rooms").join(format!("room_{}", hash_text(room))));
        t.poll_demand_fixture(&f.registry, None, Some(prepared)).await.unwrap();
        demands::wait_attempt(&f).await;
    }
    let paths = std::fs::read_to_string(f._root.path().join("shared-probe/room-paths")).unwrap();
    let paths: Vec<std::path::PathBuf> = paths.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    assert_eq!(paths, expected);
    assert_eq!(paths[0], paths[1]); // Threads share their room's files.
    assert_eq!(paths[0], paths[2]); // A later process reopens the same room.
    assert_eq!(std::fs::read_to_string(paths[0].join("room-turn-count")).unwrap(), "3");
    assert_eq!(rows(&f, "owner_shared_context_call_pins"), 3);
    f.stop().await;
}

#[tokio::test]
#[cfg(feature = "shared-intake")]
#[ignore = "requires explicit offline context probe"]
async fn native_shared_context_actual_successive_turns_remember_only_same_thread() {
    let (f, t, _) = demands::fixture_budget(2000).await;
    f.state.lock().unwrap().shared.contexts.rich = true;
    for (n, thread, body) in [(1, "$thread", "Remember the name 雪."),
        (2, "$thread", "What was the name?"), (3, "$other", "What was the name?")] {
        offer(&f, n, thread, body);
        t.poll_demand_fixture(&f.registry, None, Some(launch(&f, "text-native-session"))).await.unwrap();
        demands::wait_attempt(&f).await;
    }
    {
        let s = f.state.lock().unwrap();
        assert_eq!(s.shared.contexts.starts, 3);
        for c in s.shared.contexts.contexts.values() {
            assert_eq!(c.context.version, 3);
            assert!(c.context.history.is_empty());
        }
        let answer = |id: &str| s.shared.receipts.values().find(|r| r.work_id == id && r.body.is_some()).unwrap().body.clone().unwrap();
        assert!(answer("demand_work_2").contains("Remember the name 雪."));
        assert_eq!(answer("demand_work_3"), "Previous native inputs: ");
        assert!(s.shared.contexts.deliveries.values().any(|r| r.receipt.formatted_body.is_some()));
    }
    let inputs: Vec<String> = serde_json::from_slice(&std::fs::read(f._root.path().join("shared-probe/native-thread-1.json")).unwrap()).unwrap();
    assert_eq!(inputs.len(), 2);
    assert!(inputs[1].contains("What was the name?"));
    assert!(!inputs[1].contains("Remember the name 雪."));
    assert!(!inputs[1].contains("HTML receipt evidence"));
    assert_eq!(rows(&f, "owner_shared_context_call_pins"), 3);
    assert_eq!(rows(&f, "owner_shared_reply_deliveries"), 3);
    f.stop().await;
}

#[tokio::test]
#[cfg(feature = "shared-intake")]
#[ignore = "requires explicit offline context probe"]
async fn native_shared_context_lost_start_retains_original_hold_without_model_replay() {
    let (mut f, t, _) = demands::fixture_budget(1000).await;
    offer(&f, 1, "$thread", "Remember the name 雪.");
    f.state.lock().unwrap().shared.contexts.lose_start = true;
    t.poll_demand_fixture(&f.registry, None, Some(launch(&f, "text-context")))
        .await
        .unwrap();
    demands::wait_attempt(&f).await;
    assert_eq!(demands::count_methods(&f, "turn/start"), 0);
    assert_eq!(rows(&f, "owner_managed_calls"), 1);
    assert_eq!(rows(&f, "owner_shared_context_call_pins"), 1);
    let saved = {
        let db =
            rusqlite::Connection::open(f._root.path().join("registry/domain.sqlite3")).unwrap();
        db.query_row(
            "SELECT payload,digest,input_digest FROM owner_shared_contexts WHERE work_id='demand_work_1'",
            [],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)),
        ).unwrap()
    };
    f.registry.shutdown().await.unwrap();
    f.registry =
        OwnerResourceRegistry::isolated_for_tests(&f._root.path().join("registry")).unwrap();
    let _ = t.resume_next_demand(&f.registry, None).await;
    let restored = {
        let db =
            rusqlite::Connection::open(f._root.path().join("registry/domain.sqlite3")).unwrap();
        db.query_row(
            "SELECT payload,digest,input_digest FROM owner_shared_contexts WHERE work_id='demand_work_1'",
            [],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)),
        ).unwrap()
    };
    assert_eq!(
        saved, restored,
        "restart preserves the original rendered-input pins"
    );
    assert_eq!(rows(&f, "owner_managed_calls"), 1);
    assert_eq!(rows(&f, "owner_shared_context_call_pins"), 1);
    assert_eq!(demands::count_methods(&f, "turn/start"), 0);
    assert_eq!(f.state.lock().unwrap().shared.contexts.starts, 1);
    f.stop().await;
}
#[tokio::test]
#[cfg(feature = "shared-intake")]
#[ignore = "requires explicit offline context probe"]
async fn native_shared_context_wrong_thread_refuses_before_any_call_hold() {
    let (f, t, _) = demands::fixture_budget(1000).await;
    offer(&f, 1, "$thread", "Remember the name 雪.");
    f.state.lock().unwrap().shared.contexts.wrong_thread = true;
    t.poll_demand_fixture(&f.registry, None, Some(launch(&f, "text-context")))
        .await
        .unwrap();
    demands::wait_attempt(&f).await;
    assert_eq!(rows(&f, "owner_managed_calls"), 0);
    assert_eq!(rows(&f, "owner_shared_contexts"), 0);
    assert_eq!(demands::count_methods(&f, "turn/start"), 0);
    f.stop().await;
}

#[tokio::test]
#[cfg(feature = "shared-intake")]
#[ignore = "requires explicit offline context probe"]
async fn native_shared_context_missing_capability_refuses_before_call_or_provider_input() {
    let (f, t, _) = demands::fixture_budget(1000).await;
    offer(&f, 1, "$thread", "Current request");
    f.state.lock().unwrap().shared.contexts.v3 = false;
    t.poll_demand_fixture(&f.registry, None, Some(launch(&f, "text-native-session"))).await.unwrap();
    demands::wait_attempt(&f).await;
    assert_eq!(rows(&f, "owner_managed_calls"), 0);
    assert_eq!(rows(&f, "owner_shared_contexts"), 0);
    assert_eq!(demands::count_methods(&f, "turn/start"), 0);
    f.stop().await;
}

#[tokio::test]
#[ignore = "requires explicit offline context probe; also runs with only shared-execution enabled"]
async fn native_shared_execution_requires_v3_before_call_admission() {
    for plain_mode in [false, true] {
        let (f, t, _) = fixture().await;
        {
            let mut state = f.state.lock().unwrap();
            state.shared.contexts.v3 = plain_mode;
            state.shared.contexts.plain_mode = plain_mode;
        }
        t.poll_fixture(&f.registry, launch(&f, "text-native-session"))
            .await.unwrap();
        stopped(&f).await;
        assert_eq!(rows(&f, "owner_managed_calls"), 0);
        assert_eq!(rows(&f, "owner_shared_contexts"), 0);
        assert_eq!(f.state.lock().unwrap().shared.starts, 0);
        let methods = std::fs::read_to_string(f._root.path().join("shared-probe/methods")).unwrap();
        assert_eq!(methods.lines().filter(|method| *method == "turn/start").count(), 0);
        f.stop().await;
    }
}
