use super::*;
use serde_json::json;
use std::io::Write;

fn count(n: u64) -> Value {
    json!({"total_tokens":n*10,"input_tokens":n*7,"output_tokens":n*3,
        "cached_input_tokens":n*2,"reasoning_output_tokens":n})
}
fn receipt(response: &str, cumulative: u64) -> Value {
    json!({"type":"token_usage_record","payload":{
        "thread_id":"thread","turn_id":"turn","root_turn_id":"root",
        "session_id":"native-process","response_id":response,
        "usage":count(1),"turn_token_usage":count(cumulative),
        "thread_token_usage":count(9000)
    }})
}
fn finish() -> Value {
    json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"turn"}})
}
fn append(file: &mut tempfile::NamedTempFile, value: &Value) {
    writeln!(file, "{value}").unwrap();
    file.flush().unwrap();
}
fn fixture() -> tempfile::NamedTempFile {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    append(
        &mut file,
        &json!({"type":"session_meta","payload":{"id":"thread"}}),
    );
    file
}
async fn read_fixture(values: &[Value]) -> Result<Vec<Receipt>, Error> {
    let mut file = fixture();
    for value in values {
        append(&mut file, value);
    }
    let mut cursor = Cursor::open(file.path().to_str().unwrap(), "thread", false).await?;
    cursor.read("root", &BTreeSet::from(["turn".into()])).await
}

#[tokio::test]
async fn cold_resume_reads_only_new_original_responses_and_deduplicates_exact_ids() {
    let mut file = fixture();
    let mut old = receipt("old", 1);
    old["payload"]["turn_id"] = "old-turn".into();
    append(&mut file, &old);
    let mut cursor = Cursor::open(file.path().to_str().unwrap(), "thread", true)
        .await
        .unwrap();
    append(&mut file, &receipt("new", 1));
    append(&mut file, &receipt("new", 1));
    append(&mut file, &finish());
    let records = cursor
        .read("root", &BTreeSet::from(["turn".into()]))
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].response, "new");
    assert_eq!(records[0].usage["totalTokens"], 10);
    assert_eq!(records[0].usage["cacheWriteInputTokens"], 0);
}

#[tokio::test]
async fn persisted_usage_rejects_substituted_scope_incomplete_counters_and_conflicting_ids() {
    for field in ["thread_id", "root_turn_id"] {
        let mut changed = receipt("one", 1);
        changed["payload"][field] = "other".into();
        assert!(matches!(
            read_fixture(&[changed, finish()]).await,
            Err(Error::Scope)
        ));
    }
    for field in [
        "total_tokens",
        "input_tokens",
        "output_tokens",
        "cached_input_tokens",
        "reasoning_output_tokens",
    ] {
        let mut missing = receipt("one", 1);
        missing["payload"]["usage"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(matches!(
            read_fixture(&[missing, finish()]).await,
            Err(Error::Malformed)
        ));
    }
    let mut changed = receipt("one", 2);
    changed["payload"]["usage"] = count(2);
    assert!(matches!(
        read_fixture(&[receipt("one", 1), changed, finish()]).await,
        Err(Error::Scope)
    ));
    let file = fixture();
    assert!(matches!(
        Cursor::open(file.path().to_str().unwrap(), "other-thread", true).await,
        Err(Error::Scope)
    ));
}

#[tokio::test]
async fn persisted_usage_requires_complete_turns_and_unbroken_per_turn_receipt_chain() {
    for values in [
        vec![receipt("first-missing", 2), finish()],
        vec![receipt("first", 1), receipt("gap", 3), finish()],
        vec![finish(), receipt("late", 1)],
    ] {
        assert!(matches!(read_fixture(&values).await, Err(Error::Scope)));
    }
    for values in [vec![receipt("unfinished", 1)], vec![finish()]] {
        assert!(matches!(read_fixture(&values).await, Err(Error::State)));
    }
    let mut file = fixture();
    append(&mut file, &receipt("one", 1));
    append(&mut file, &finish());
    let mut cursor = Cursor::open(file.path().to_str().unwrap(), "thread", false)
        .await
        .unwrap();
    assert!(matches!(
        cursor
            .read(
                "root",
                &BTreeSet::from(["turn".into(), "missing-child-turn".into()])
            )
            .await,
        Err(Error::State)
    ));
}

#[tokio::test]
async fn persisted_usage_rejects_truncated_or_oversized_records_and_rewritten_files() {
    for (bytes, expected) in [
        (
            b"{\"type\":\"token_usage_record\"".to_vec(),
            Error::Malformed,
        ),
        (vec![b'x'; MAX_LINE + 1], Error::Capacity),
    ] {
        let mut file = fixture();
        let mut cursor = Cursor::open(file.path().to_str().unwrap(), "thread", true)
            .await
            .unwrap();
        file.write_all(&bytes).unwrap();
        file.flush().unwrap();
        assert!(
            cursor
                .read("root", &BTreeSet::from(["turn".into()]))
                .await
                .is_err_and(|error| error == expected)
        );
    }
    let file = fixture();
    let mut cursor = Cursor::open(file.path().to_str().unwrap(), "thread", true)
        .await
        .unwrap();
    file.as_file().set_len(0).unwrap();
    assert!(matches!(
        cursor.read("root", &BTreeSet::from(["turn".into()])).await,
        Err(Error::Scope)
    ));
}
