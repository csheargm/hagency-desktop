use super::*;
#[test]
fn shared_execution_and_work_match_central_canonical_vectors() {
    let runtime: serde_json::Value =
        serde_json::from_str(include_str!("management-shared-execution-v1.json")).unwrap();
    let command: SharedRuntimeCommand = serde_json::from_value(runtime["command"].clone()).unwrap();
    command
        .validate(&command.reserve_command.manager_mxid)
        .unwrap();
    assert_eq!(
        canonical::digest(&serde_json::to_value(&command).unwrap()).unwrap(),
        runtime["commandDigest"]
    );
    let receipt: SharedProcessReceipt = serde_json::from_value(runtime["receipt"].clone()).unwrap();
    receipt.validate_for(&command).unwrap();
    assert_eq!(
        canonical::digest(&serde_json::to_value(&receipt).unwrap()).unwrap(),
        runtime["receiptDigest"]
    );
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("management-shared-work-v1.json")).unwrap();
    let work: SharedWorkItem = serde_json::from_value(fixture["work"].clone()).unwrap();
    work.validate().unwrap();
    assert_eq!(
        canonical::digest(&serde_json::to_value(&work).unwrap()).unwrap(),
        fixture["workDigest"]
    );
    let receipt: SharedWorkReceipt = serde_json::from_value(fixture["receipt"].clone()).unwrap();
    receipt
        .validate_for(&work, &receipt.process_digest)
        .unwrap();
    assert_eq!(
        canonical::digest(&serde_json::to_value(&receipt).unwrap()).unwrap(),
        fixture["receiptDigest"]
    );
}
#[test]
fn shared_wire_rejects_changed_work_and_oversized_escaped_receipt() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("management-shared-work-v1.json")).unwrap();
    let mut work: SharedWorkItem = serde_json::from_value(fixture["work"].clone()).unwrap();
    work.body.push('!');
    assert!(work.validate().is_err());
    let work: SharedWorkItem = serde_json::from_value(fixture["work"].clone()).unwrap();
    let mut receipt: SharedWorkReceipt =
        serde_json::from_value(fixture["receipt"].clone()).unwrap();
    // The current wire ceiling is 48 KiB; the raw body still fits 32 KiB.
    receipt.body = Some("\"".repeat(24 * 1024));
    use sha2::{Digest, Sha256};
    receipt.body_digest = Some(format!(
        "{:x}",
        Sha256::digest(receipt.body.as_ref().unwrap().as_bytes())
    ));
    assert!(
        receipt
            .validate_for(&work, &receipt.process_digest)
            .is_err()
    );
    let mut unknown = fixture["work"].clone();
    unknown["authority"] = json!(true);
    assert!(serde_json::from_value::<SharedWorkItem>(unknown).is_err());
}

#[test]
fn shared_room_receipt_is_bound_to_exact_assignment_room_and_generation() {
    let fixture: serde_json::Value=serde_json::from_str(include_str!("management-shared-work-v1.json")).unwrap();
    let mut work:SharedWorkItem=serde_json::from_value(fixture["work"].clone()).unwrap();
    work.room_binding=Some(SharedRoomBinding{id:"srb_team".into(),generation:1,assignment_id:work.assignment_id.clone(),room_id:work.room_id.clone(),space_id:"!space:example.test".into()});
    work.validate().unwrap();
    let valid=work.clone();
    work.room_binding.as_mut().unwrap().room_id="!other:example.test".into();assert!(work.validate().is_err());
    work=valid.clone();work.room_binding.as_mut().unwrap().assignment_id="another_agent".into();assert!(work.validate().is_err());
    work=valid;work.room_binding.as_mut().unwrap().generation=0;assert!(work.validate().is_err());
}

#[test]
fn shared_claude_root_cannot_relabel_legacy_codex_or_broaden_owner_policy() {
    let fixture: serde_json::Value=serde_json::from_str(include_str!("management-shared-execution-v1.json")).unwrap();
    let original: SharedRuntimeCommand=serde_json::from_value(fixture["command"].clone()).unwrap();
    let mut claude=original.clone();
    claude.version=4;claude.capability=SHARED_CLAUDE_CAPABILITY.into();claude.policy=SHARED_CLAUDE_POLICY.into();
    claude.validate(&claude.reserve_command.manager_mxid).unwrap();
    assert_eq!(claude.runtime().unwrap(), "claude");
    assert_eq!(claude.execution_policy(true,Some("standard")).unwrap(),"claude_tools_standard_web_v1");
    assert!(claude.execution_policy(true,Some("arbitrary")).is_err());
    assert_ne!(canonical::digest(&serde_json::to_value(&claude).unwrap()).unwrap(), fixture["commandDigest"]);
    assert_eq!(canonical::digest(&serde_json::to_value(&original).unwrap()).unwrap(), fixture["commandDigest"]);
    assert_eq!(canonical::digest(&serde_json::to_value(&claude.reserve_command).unwrap()).unwrap(), original.reserve_command_digest);
    for field in ["version","capability","policy"] {
        let mut mixed=serde_json::to_value(&claude).unwrap();
        mixed[field]=fixture["command"][field].clone();
        assert!(serde_json::from_value::<SharedRuntimeCommand>(mixed).unwrap().validate(&original.reserve_command.manager_mxid).is_err(),"{field}");
    }
}
