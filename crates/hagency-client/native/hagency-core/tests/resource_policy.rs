use hagency_core::{canonical, project::Resource};
use serde_json::json;

fn original() -> serde_json::Value {
    json!({"presetId":"legacy", "seatId":"seat", "framework":"codex",
        "model":"gpt-5.6-sol", "provider":null, "reasoning":"medium",
        "roles":[], "ceiling":{"tokens":5000,"period":"monthly"}, "published":true})
}

#[test]
fn resource_policy_absence_preserves_original_shape_and_digest() {
    let old = original();
    let resource: Resource = serde_json::from_value(old.clone()).unwrap();
    resource.validate().unwrap();
    assert!(resource.execution_policy.is_none());
    let encoded = serde_json::to_value(resource).unwrap();
    assert_eq!(encoded, old);
    assert_eq!(canonical::digest(&encoded).unwrap(), canonical::digest(&old).unwrap());
}

#[test]
fn resource_policy_is_typed_closed_and_framework_checked() {
    for yolo in [false, true] {
        let mut value = original();
        value["executionPolicy"] = json!({"yolo":yolo});
        let resource: Resource = serde_json::from_value(value.clone()).unwrap();
        resource.validate().unwrap();
        assert_eq!(serde_json::to_value(&resource).unwrap(), value);
        // Full trust exists for Codex and Claude Code; no other tool takes it.
        let mut claude = resource.clone();
        claude.framework = "claude".into();
        claude.validate().unwrap();
        let mut other = resource;
        other.framework = "octos".into();
        assert_eq!(other.validate().is_err(), yolo);
    }
    for invalid in [json!({}), json!({"yolo":"true"}), json!({"yolo":true,"approval":"never"})] {
        let mut value = original();
        value["executionPolicy"] = invalid;
        assert!(serde_json::from_value::<Resource>(value).is_err());
    }
}
