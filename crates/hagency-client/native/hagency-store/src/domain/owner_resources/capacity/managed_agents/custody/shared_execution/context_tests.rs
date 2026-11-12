use super::*;
fn fixture() -> (serde_json::Value, SharedWorkItem, SharedContextRecord) {
    let v: serde_json::Value = serde_json::from_str(include_str!("management-shared-native-context-v3.json")).unwrap();
    let w = serde_json::from_value(v["work"].clone()).unwrap();
    let c = SharedContextRecord { context: serde_json::from_value(v["context"].clone()).unwrap(),
        context_digest: v["contextDigest"].as_str().unwrap().into(), input_digest: v["inputDigest"].as_str().unwrap().into() };
    (v,w,c)
}
#[test]
fn shared_context_v3_matches_server_bytes_and_rejects_history_reinjection() {
    let (v,w,c) = fixture();
    assert_eq!(c.render_for(&w).unwrap(),v["renderedInput"]);
    for (key,value) in [("version",serde_json::json!(1)), ("version",serde_json::json!(2)),
        ("inputProfile",serde_json::json!("shared_thread_text_v2")), ("coverage",serde_json::json!("prior_delivered_pairs_only")),
        ("history",serde_json::json!([{"body":"repeat"}])), ("omittedEarlier",serde_json::json!(false)),
        ("roomId",serde_json::json!("!foreign:test")), ("workDigest",serde_json::json!("0".repeat(64))),
        ("threadRoot",serde_json::json!("$foreign")), ("controlRevision",serde_json::json!(99))] {
        let mut changed = v["context"].clone(); changed[key]=value;
        let changed: SharedThreadContext = serde_json::from_value(changed).unwrap();
        assert!(render_context(&changed,&w).is_err(),"{key}");
    }
    let mut old = v["context"].clone(); old["budget"] = serde_json::json!({"modelContextWindow":128000});
    assert!(serde_json::from_value::<SharedThreadContext>(old).is_err());
    let mut tampered = c; tampered.input_digest="0".repeat(64); assert!(tampered.render_for(&w).is_err());
}
#[test]
fn shared_context_closed_modes_cannot_infer_plaintext_authority() {
    for mode in ["", "unknown", "thread_context_v2"] {
        assert!(SharedWorkInputMode { mode:mode.into(),context_digest:None,input_digest:None }.validate().is_err());
    }
    assert!(SharedWorkInputMode { mode:"plain_text_v1".into(),context_digest:Some("0".repeat(64)),input_digest:None }.validate().is_err());
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
    let v: serde_json::Value = serde_json::from_str(include_str!("management-shared-context-insertion-order-v1.json")).unwrap();
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
