//! A native pre-input diagnostic is the sole evidence that a resume may be
//! rebuilt. Transport loss and later execution failures never authorize replay.
use serde_json::Value;

pub(super) fn exact(expected: Option<&str>, actual: &str, v: &Value) -> bool {
    let Some(expected) = expected else {
        return false;
    };
    expected == actual
        && v["session_id"] == expected
        && v["subtype"] == "error_during_execution"
        && v["is_error"] == true
        && v["num_turns"].as_u64() == Some(0)
        && v["duration_api_ms"].as_u64() == Some(0)
        && v["total_cost_usd"].as_f64() == Some(0.0)
        && v["modelUsage"].as_object().is_some_and(|v| v.is_empty())
        && v["permission_denials"]
            .as_array()
            .is_some_and(|v| v.is_empty())
        && v["result"].as_str().is_none_or(str::is_empty)
        && v["errors"]
            == serde_json::json!([format!("No conversation found with session ID: {expected}")])
        && [
            "input_tokens",
            "output_tokens",
            "cache_creation_input_tokens",
            "cache_read_input_tokens",
        ]
        .iter()
        .all(|k| v["usage"][k].as_u64() == Some(0))
        && zero_numbers(&v["usage"])
}
fn zero_numbers(v: &Value) -> bool {
    match v {
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::Object(v) => v.values().all(zero_numbers),
        Value::Array(v) => v.iter().all(zero_numbers),
        _ => true,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_resume_requires_exact_pre_input_native_refusal() {
        let v = serde_json::json!({"session_id":"saved","subtype":"error_during_execution",
            "is_error":true,"num_turns":0,"duration_api_ms":0,"total_cost_usd":0,
            "modelUsage":{},"permission_denials":[],"errors":["No conversation found with session ID: saved"],
            "usage":{"input_tokens":0,"output_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}});
        assert!(exact(Some("saved"), "saved", &v));
        assert!(!exact(None, "saved", &v));
        assert!(!exact(Some("other"), "saved", &v));
        for (pointer, value) in [
            ("/num_turns", 1.into()),
            ("/duration_api_ms", 1.into()),
            ("/total_cost_usd", 1.into()),
            ("/usage/output_tokens", 1.into()),
            ("/errors", serde_json::json!(["other error"])),
            ("/session_id", "other".into()),
            ("/modelUsage", serde_json::json!({"model":{}})),
            ("/is_error", false.into()),
        ] {
            let mut altered = v.clone();
            *altered.pointer_mut(pointer).unwrap() = value;
            assert!(!exact(Some("saved"), "saved", &altered), "{pointer}");
        }
        let mut used = v;
        used["usage"]["server_tool_use"] = serde_json::json!({"web_search_requests":1});
        assert!(!exact(Some("saved"), "saved", &used));
    }
}
