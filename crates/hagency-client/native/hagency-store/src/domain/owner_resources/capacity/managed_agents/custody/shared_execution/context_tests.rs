use super::*;
#[test]
fn shared_context_v3_matches_server_bytes_and_rejects_history_reinjection() {
    let v: serde_json::Value = serde_json::from_str(include_str!("management-shared-native-context-v3.json")).unwrap();
    let w: SharedWorkItem = serde_json::from_value(v["work"].clone()).unwrap();
    let c = SharedContextRecord {
        context: serde_json::from_value(v["context"].clone()).unwrap(),
        context_digest: v["contextDigest"].as_str().unwrap().into(),
        input_digest: v["inputDigest"].as_str().unwrap().into(),
    };
    assert_eq!(c.render_for(&w).unwrap(), v["renderedInput"]);
    assert!(c.context.history.is_empty() && c.context.omitted_earlier && c.context.budget.is_none());
    let (_, _, old) = fixture();
    let mut repeated = c.context.clone();
    repeated.history = old.context.history;
    assert!(render_context(&repeated, &w).is_err());
    for (key, value) in [("inputProfile", json!(INPUT_PROFILE_V2)),
        ("coverage", json!(COVERAGE_PRIOR_DELIVERED_PAIRS)), ("omittedEarlier", json!(false)),
        ("roomId", json!("!other:test")), ("version", json!(2))] {
        let mut changed = v["context"].clone();
        changed[key] = value;
        assert!(render_context(&serde_json::from_value(changed).unwrap(), &w).is_err(), "{key}");
    }
    assert!(SharedContextBudgetInput { version: 3, model_context_window: Some(128_000) }.validate().is_err());
    let mut tampered = c;
    tampered.input_digest = "f".repeat(64);
    assert!(tampered.render_for(&w).is_err());
}
fn fixture() -> (serde_json::Value, SharedWorkItem, SharedContextRecord) {
    let v: serde_json::Value =
        serde_json::from_str(include_str!("management-shared-thread-context-v1.json")).unwrap();
    let w = serde_json::from_value(v["work"].clone()).unwrap();
    let c = SharedContextRecord {
        context: serde_json::from_value(v["context"].clone()).unwrap(),
        context_digest: v["contextDigest"].as_str().unwrap().into(),
        input_digest: v["inputDigest"].as_str().unwrap().into(),
    };
    (v, w, c)
}
#[test]
fn shared_context_canonical_renderer_matches_independent_server_bytes() {
    let (v, w, c) = fixture();
    let input = c.render_for(&w).unwrap();
    assert_eq!(input, v["renderedInput"]);
    assert_eq!(input.len(), 1830);
    assert!(!input.ends_with('\n'));
    let d = SharedDeliveryRecord {
        receipt: serde_json::from_value(v["delivery"].clone()).unwrap(),
        receipt_digest: v["deliveryDigest"].as_str().unwrap().into(),
    };
    d.validate_for(&serde_json::from_value(v["previousWork"].clone()).unwrap())
        .unwrap();
}
#[test]
fn shared_context_rejects_cross_thread_assignment_work_and_changed_evidence() {
    let (_, w, c) = fixture();
    for field in [
        "roomId",
        "threadRoot",
        "assignmentId",
        "workDigest",
        "runtimeCommandDigest",
    ] {
        let mut changed = serde_json::to_value(&c.context).unwrap();
        changed[field] = "changed".into();
        let changed: SharedThreadContext = serde_json::from_value(changed).unwrap();
        assert!(changed.validate_for(&w).is_err(), "{field}");
    }
    let mut changed = c.context.clone();
    changed.history[0].delivery.body.push('!');
    assert!(changed.validate_for(&w).is_err());
    let mut changed = c.context.clone();
    changed.history[0].user_body.push('!');
    assert!(changed.validate_for(&w).is_err());
    let mut changed = c.context.clone();
    changed.history[0].received_at_ms = w.created_at_ms + 1;
    assert!(changed.validate_for(&w).is_err());
    let mut changed = c.context.clone();
    changed.history.push(changed.history[0].clone());
    assert!(changed.validate_for(&w).is_err());
    let mut changed = c;
    changed.input_digest = "a".repeat(64);
    assert!(changed.render_for(&w).is_err());
}
#[test]
fn shared_context_closed_modes_and_sent_delivery_are_not_inferred() {
    let (v, w, c) = fixture();
    let mut unknown = v["context"].clone();
    unknown["executionAllowed"] = true.into();
    assert!(serde_json::from_value::<SharedThreadContext>(unknown).is_err());
    for m in [
        SharedWorkInputMode {
            mode: "unknown".into(),
            context_digest: None,
            input_digest: None,
        },
        SharedWorkInputMode {
            mode: "plain_text_v1".into(),
            context_digest: Some(c.context_digest.clone()),
            input_digest: Some(c.input_digest.clone()),
        },
        SharedWorkInputMode {
            mode: "thread_context_v1".into(),
            context_digest: Some(c.context_digest),
            input_digest: None,
        },
    ] {
        assert!(m.validate().is_err())
    }
    let o = SharedReplyObservation {
        state: Some("sent".into()),
        delivery: None,
    };
    assert!(o.validate_for(&w).is_err());
    let o = SharedReplyObservation {
        state: Some("completed".into()),
        delivery: None,
    };
    assert!(o.validate_for(&w).is_err());
}
#[test]
fn shared_context_queued_followup_preserves_explicit_delivery_time_without_mutation() {
    let (_, w, c) = fixture();
    let mut history = c.context.clone();
    history.history[0].delivery.sent_at_ms = w.created_at_ms + 100;
    history.history[0].delivery_digest =
        context_types::context_digest(&history.history[0].delivery).unwrap();
    history.validate_for(&w).unwrap();
    assert_ne!(
        render_context(&history, &w).unwrap(),
        c.render_for(&w).unwrap()
    );
    assert_eq!(c.context.history[0].delivery.sent_at_ms, 1500);
}

#[test]
fn shared_context_real_server_ordered_reply_preserves_original_context_bytes() {
    let v: serde_json::Value = serde_json::from_str(include_str!(
        "management-shared-context-insertion-order-v1.json"
    ))
    .unwrap();
    let delivery: SharedReplyDelivery = serde_json::from_value(v["delivery"].clone()).unwrap();
    assert_eq!(
        context_types::context_hash(v["matrixContent"].as_str().unwrap()),
        delivery.matrix_content_digest
    );
    delivery
        .validate()
        .expect("Actual server delivery must not require canonical outbound key order");
    let w: SharedWorkItem = serde_json::from_value(v["work"].clone()).unwrap();
    let c = SharedContextRecord {
        context: serde_json::from_value(v["context"].clone()).unwrap(),
        context_digest: v["contextDigest"].as_str().unwrap().into(),
        input_digest: v["inputDigest"].as_str().unwrap().into(),
    };
    c.render_for(&w).unwrap();
    let record = SharedDeliveryRecord {
        receipt: delivery.clone(),
        receipt_digest: v["deliveryDigest"].as_str().unwrap().into(),
    };
    record
        .validate_for(&serde_json::from_value(v["previousWork"].clone()).unwrap())
        .unwrap();
    let mut changed = delivery.clone();
    changed.body.push('!');
    changed.body_digest = context_types::context_hash(&changed.body);
    assert!(
        changed.validate().is_err(),
        "Changing body and its hash cannot preserve the outbound-content digest"
    );
    let mut changed = delivery.clone();
    changed.thread_root = "$different-thread".into();
    assert!(
        changed.validate().is_err(),
        "Changing thread cannot preserve the outbound-content digest"
    );
    let mut changed = delivery;
    changed.matrix_content_digest = "a".repeat(64);
    assert!(
        changed.validate().is_err(),
        "Unknown outbound-content digest must still be refused"
    );
}

#[test]
fn shared_context_ordered_reply_handles_escaped_unicode_without_accepting_changed_content() {
    let (v, _, _) = fixture();
    let mut delivery: SharedReplyDelivery = serde_json::from_value(v["delivery"].clone()).unwrap();
    let content = r#"{"msgtype":"m.text","body":"中文 \"quote\" \\slash\nnext","m.relates_to":{"rel_type":"m.thread","event_id":"$ordered-thread","is_falling_back":true,"m.in_reply_to":{"event_id":"$ordered-thread"}}}"#;
    let value: serde_json::Value = serde_json::from_str(content).unwrap();
    delivery.body = value["body"].as_str().unwrap().into();
    delivery.body_digest = context_types::context_hash(&delivery.body);
    delivery.thread_root = "$ordered-thread".into();
    delivery.matrix_content_digest = context_types::context_hash(content);
    delivery.validate().unwrap();
    let mut altered = value;
    altered["m.relates_to"]["is_falling_back"] = false.into();
    delivery.matrix_content_digest = context_types::context_digest(&altered).unwrap();
    assert!(delivery.validate().is_err());
}

#[test]
fn shared_context_rich_reply_preserves_exact_html_and_rejects_changed_provenance() {
    let (_, w, mut c) = fixture();
    let d = &mut c.context.history[0].delivery;
    d.body = "**中文**\n\"quote\" \\slash".into();
    d.body_digest = context_types::context_hash(&d.body);
    // Exact outbound bytes from the server's insertion-order writer. The HTML
    // is evidence, not a request to run the current Markdown renderer.
    let content = r#"{"msgtype":"m.text","body":"**中文**\n\"quote\" \\slash","m.relates_to":{"rel_type":"m.thread","event_id":"$thread","is_falling_back":true,"m.in_reply_to":{"event_id":"$thread"}},"format":"org.matrix.custom.html","formatted_body":"<p><strong>中文</strong>\n&amp; evidence</p>\n"}"#;
    let value: serde_json::Value = serde_json::from_str(content).unwrap();
    d.formatted_body = Some(value["formatted_body"].as_str().unwrap().into());
    d.matrix_content_digest = context_types::context_hash(content);
    d.validate().unwrap();
    for field in ["body", "msgtype", "format", "formatted_body", "m.relates_to", "unexpected"] {
        let mut changed = value.clone();
        changed[field] = "changed".into();
        let mut delivery = d.clone();
        delivery.matrix_content_digest = context_types::context_digest(&changed).unwrap();
        assert!(delivery.validate().is_err(), "{field}");
    }
    let mut changed = d.clone();
    changed.formatted_body.as_mut().unwrap().push('!');
    assert!(changed.validate().is_err());
    let mut changed = d.clone();
    changed.formatted_body = None;
    assert!(changed.validate().is_err());
    // Canonical key order remains accepted without rewriting the receipt.
    d.matrix_content_digest = context_types::context_digest(&value).unwrap();
    d.validate().unwrap();
    c.context.history[0].delivery_digest = context_types::context_digest(d).unwrap();
    c.context_digest = context_types::context_digest(&c.context).unwrap();
    c.input_digest = context_types::context_hash(&render_context(&c.context, &w).unwrap());
    let text = c.render_for(&w).unwrap();
    assert!(!text.contains("formattedBody"));
    assert!(!text.contains("<strong>"));
    assert!(text.contains("**中文**"));
}

#[test]
fn shared_context_plain_fallback_keeps_full_markdown_and_original_receipt_shape() {
    let (_, w, mut c) = fixture();
    let d = &mut c.context.history[0].delivery;
    d.body = "**<&>**\n".repeat(2500);
    d.body_digest = context_types::context_hash(&d.body);
    let content = json!({"msgtype":"m.text","body":d.body,"m.relates_to":{"rel_type":"m.thread","event_id":d.thread_root,"is_falling_back":true,"m.in_reply_to":{"event_id":d.thread_root}}});
    d.matrix_content_digest = context_types::context_digest(&content).unwrap();
    d.validate().unwrap();
    assert!(serde_json::to_value(&d).unwrap().get("formattedBody").is_none());
    c.context.history[0].delivery_digest = context_types::context_digest(d).unwrap();
    c.context_digest = context_types::context_digest(&c.context).unwrap();
    c.input_digest = context_types::context_hash(&render_context(&c.context, &w).unwrap());
    c.render_for(&w).unwrap();
    assert_eq!(c.context.history[0].delivery.body.len(), 20_000);
}

#[test]
fn shared_context_v2_renderer_matches_independent_server_bytes_and_recomputes_budget() {
    let v: serde_json::Value =
        serde_json::from_str(include_str!("management-shared-thread-context-v2.json")).unwrap();
    let w: SharedWorkItem = serde_json::from_value(v["work"].clone()).unwrap();
    let c = SharedContextRecord {
        context: serde_json::from_value(v["context"].clone()).unwrap(),
        context_digest: v["contextDigest"].as_str().unwrap().into(),
        input_digest: v["inputDigest"].as_str().unwrap().into(),
    };
    let input = c.render_for(&w).unwrap();
    assert_eq!(input, v["renderedInput"]);
    assert_eq!(input.len(), v["inputBytes"].as_u64().unwrap() as usize);
    assert!(input.starts_with("Answer currentRequest only. history holds 1 of 3 earlier delivered exchanges from this thread in time order; the oldest were omitted to fit the model budget; 1 redacted exchanges are excluded. "));
    assert!(input.contains("\"editedBody\""));
    let (window, source) = budget_window(Some(272_000));
    assert_eq!((window, source), (272_000, WINDOW_SOURCE_OBSERVED));
    assert_eq!(history_budget_bytes(window), c.context.budget.as_ref().unwrap().history_budget_bytes);
    assert_eq!(budget_window(Some(1_000)), (DEFAULT_MODEL_CONTEXT_WINDOW, WINDOW_SOURCE_DEFAULT));
    assert_eq!(history_budget_bytes(DEFAULT_MODEL_CONTEXT_WINDOW), 31_999);
    assert_eq!(history_budget_bytes(10_000_000), MAX_HISTORY_BYTES_V2 as u64);
    let reject = |mutate: &dyn Fn(&mut SharedThreadContext)| {
        let mut bad = c.context.clone();
        mutate(&mut bad);
        assert!(render_context(&bad, &w).is_err());
    };
    // No event twice: not the current request, not another pair's events.
    reject(&|c| c.history[0].event_id = w.event_id.clone());
    reject(&|c| c.history[0].delivery.matrix_event_id = c.history[0].event_id.clone());
    reject(&|c| c.history[0].edited_event_id = Some(c.history[0].event_id.clone()));
    reject(&|c| {
        let mut again = c.history[0].clone();
        again.work_id = "swork_again".into();
        again.received_at_ms += 1;
        c.history.push(again);
        c.budget.as_mut().unwrap().included_pairs = 2;
    });
    // The budget and coverage are recomputed, never trusted from the wire.
    reject(&|c| c.budget.as_mut().unwrap().history_budget_bytes += 1);
    reject(&|c| c.budget.as_mut().unwrap().model_context_window = 1_000);
    reject(&|c| c.budget.as_mut().unwrap().window_source = "guess".into());
    reject(&|c| c.budget.as_mut().unwrap().included_pairs = 0);
    reject(&|c| c.omitted_earlier = false);
    reject(&|c| c.budget = None);
    reject(&|c| c.version = 1);
    reject(&|c| c.history[0].edited_body = Some("other".into()));
    reject(&|c| c.history[0].edited_body_digest = None);
    // Version 1 records keep their exact bytes and refuse the new fields.
    let (v1, w1, c1) = fixture();
    assert_eq!(c1.render_for(&w1).unwrap(), v1["renderedInput"]);
    let mut smuggled = c1.context.clone();
    smuggled.budget = c.context.budget.clone();
    assert!(render_context(&smuggled, &w1).is_err());
    let mut smuggled = c1.context;
    smuggled.history[0].edited_event_id = Some("$prior_edit".into());
    smuggled.history[0].edited_body = Some("x".into());
    smuggled.history[0].edited_body_digest = Some(context_types::context_hash("x"));
    assert!(render_context(&smuggled, &w1).is_err());
    // The freeze request itself is bounded.
    assert!(SharedContextBudgetInput { version: 2, model_context_window: None }.validate().is_ok());
    assert!(SharedContextBudgetInput { version: 2, model_context_window: Some(272_000) }.validate().is_ok());
    assert!(SharedContextBudgetInput { version: 1, model_context_window: None }.validate().is_err());
    assert!(SharedContextBudgetInput { version: 2, model_context_window: Some(0) }.validate().is_err());
}
