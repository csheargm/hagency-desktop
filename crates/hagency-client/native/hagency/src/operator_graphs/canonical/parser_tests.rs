use super::*;

#[test]
fn current_graph_json_preserves_fractional_large_and_opaque_values() {
    let raw = r#"{"score":0.75,"large":18446744073709551615,"negative":-42,"__proto__":{"label":"原件"},"items":[null,true,false,"text",{"result":1.25}]}"#;
    let value = parse_draft_json(raw).unwrap();
    assert_eq!(value, serde_json::from_str::<Value>(raw).unwrap());
    assert_eq!(value["large"].as_u64(), Some(u64::MAX));
    assert_eq!(value["score"].as_f64(), Some(0.75));
    assert_eq!(value["__proto__"]["label"], "原件");
    assert_eq!(parse_draft_json("null").unwrap(), Value::Null);
}

#[test]
fn current_graph_json_rejects_ambiguous_and_malformed_drafts() {
    for raw in [
        r#"{"status":"active","status":"complete"}"#,
        r#"{"result":{"approved":true,"approved":false}}"#,
        r#"[{"value":1,"value":2}]"#,
        r#"{"a":1,"\u0061":2}"#,
        "",
        "{} null",
        "[1,]",
        "NaN",
        "1e400",
    ] {
        assert!(parse_draft_json(raw).is_err(), "accepted ambiguous or malformed draft: {raw}");
    }
    let deep = format!("{}0{}", "[".repeat(128), "]".repeat(128));
    assert!(parse_draft_json(&deep).is_err(), "serde recursion limit must stay enabled");
}

#[test]
fn current_graph_json_enforces_byte_and_collection_boundaries() {
    let boundary = format!("\"{}\"", "x".repeat(MAX_DRAFT_JSON_BYTES - 2));
    assert_eq!(boundary.len(), MAX_DRAFT_JSON_BYTES);
    assert!(parse_draft_json(&boundary).is_ok());
    assert!(matches!(parse_draft_json(&(boundary + " ")), Err(Error::Capacity)));
    let multibyte = format!("\"{}\"", "中".repeat(MAX_DRAFT_JSON_BYTES / 3 + 1));
    assert!(matches!(parse_draft_json(&multibyte), Err(Error::Capacity)));

    for count in [MAX_DRAFT_JSON_ITEMS, MAX_DRAFT_JSON_ITEMS + 1] {
        let array = format!("[{}]", vec!["0"; count].join(","));
        let object = format!("{{{}}}", (0..count).map(|index| format!("\"k{index}\":0")).collect::<Vec<_>>().join(","));
        for raw in [array, object] {
            assert!(raw.len() < MAX_DRAFT_JSON_BYTES);
            assert_eq!(parse_draft_json(&raw).is_ok(), count == MAX_DRAFT_JSON_ITEMS);
        }
    }
}
