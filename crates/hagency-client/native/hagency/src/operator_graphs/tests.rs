use super::*;

#[test]
fn live_graph_store_participates_in_installation_exclusion() {
    use hagency_platform::legacy_writer::{ExclusiveWriterGuard, Error};
    let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let store = GraphStore::open(root.path()).unwrap();
    store.create(&minimal_graph()).unwrap();
    assert!(matches!(ExclusiveWriterGuard::acquire(), Err(Error::Locked)));
    drop(store);
    // This test runs alone for the before/after race probe. Other unit tests may
    // hold the cooperative installation lock during a normal parallel run.
}

#[test]
fn failed_graph_open_releases_installation_guard_without_source_effects() {
    use hagency_platform::legacy_writer::{ExclusiveWriterGuard, LegacyWriterAdmission, LegacyWriterGuard};
    let temp = tempfile::tempdir().unwrap();
    let authority = temp.path().join("authority");
    let source = temp.path().join("source");
    hagency_store::private::directory(&source).unwrap();
    hagency_store::private::replace(&source.join(GRAPHS_FILE), b"invalid").unwrap();
    let guard = LegacyWriterGuard::isolated_for_tests(&authority).unwrap();
    assert!(GraphStore::with_writer(&source, LegacyWriterAdmission::Covered(guard)).is_err());
    let _exclusive = ExclusiveWriterGuard::isolated_for_tests(&authority).unwrap();
    assert_eq!(std::fs::read(source.join(GRAPHS_FILE)).unwrap(), b"invalid");
    assert_eq!(std::fs::read_dir(source).unwrap().count(), 1);
}

#[test]
fn graph_writer_lifetime_and_restart_cover_all_document_effects() {
    use hagency_platform::legacy_writer::{Error, ExclusiveWriterGuard, LegacyWriterAdmission, LegacyWriterGuard};
    let temp = tempfile::tempdir().unwrap();
    let authority = temp.path().join("authority");
    let source = temp.path().join("source");
    hagency_store::private::directory(&source).unwrap();
    let guard = LegacyWriterGuard::isolated_for_tests(&authority).unwrap();
    let store = GraphStore::with_writer(&source, LegacyWriterAdmission::Covered(guard)).unwrap();
    for stage in 0..4 {
        match stage {
            0 => { store.create(&minimal_graph()).unwrap(); }
            1 => { store.advance("retained").unwrap(); }
            2 => { store.update_node("retained", "a", &json!({"status":"active"})).unwrap(); }
            _ => { store.delete("retained").unwrap(); }
        }
        assert!(matches!(ExclusiveWriterGuard::isolated_for_tests(&authority), Err(Error::Locked)));
    }
    let files = [GRAPHS_FILE, MESSAGES_FILE, COUNTER_FILE];
    let before: Vec<_> = files.iter().map(|name| std::fs::read(source.join(name)).unwrap()).collect();
    drop(store);
    let exclusive = ExclusiveWriterGuard::isolated_for_tests(&authority).unwrap();
    assert!(matches!(LegacyWriterGuard::isolated_for_tests(&authority), Err(Error::Locked)));
    drop(exclusive);
    let marker = authority.join("legacy-resource-refusal.json");
    hagency_store::private::write_new(&marker, b"obsolete migration state").unwrap();
    let guard = LegacyWriterGuard::isolated_for_tests(&authority).unwrap();
    let reopened = GraphStore::with_writer(&source, LegacyWriterAdmission::Covered(guard)).unwrap();
    assert!(matches!(ExclusiveWriterGuard::isolated_for_tests(&authority), Err(Error::Locked)));
    assert_eq!(std::fs::read(marker).unwrap(), b"obsolete migration state");
    drop(reopened);
    for (name, bytes) in files.iter().zip(before) {
        assert_eq!(std::fs::read(source.join(name)).unwrap(), bytes);
    }
}

fn minimal_graph() -> Value {
    json!({"id":"retained", "owner":"operator", "label":"Saved work",
        "nodes":{"a":{"assignee":"alpha", "description":"Inspect"}}})
}

#[test]
fn invalid_documents_are_refused_without_rewriting_original_bytes() {
    for (name, bytes) in [
        (GRAPHS_FILE, b"[".as_slice()),
        (GRAPHS_FILE, b"[]".as_slice()),
        (GRAPHS_FILE, br#"{"retained":{"id":"retained"}}"#.as_slice()),
        (MESSAGES_FILE, b"{}".as_slice()),
        (COUNTER_FILE, b"-1".as_slice()),
    ] {
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let path = root.path().join(name);
        hagency_store::private::replace(&path, bytes).unwrap();
        assert!(GraphStore::open(root.path()).is_err(), "{name}");
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }
}

#[test]
fn oversized_documents_and_duplicate_normalized_ids_are_refused() {
    let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let path = root.path().join(GRAPHS_FILE);
    let file = hagency_store::private::open(&path, true).unwrap();
    file.set_len(MAX_GRAPH_BYTES as u64 + 1).unwrap();
    drop(file);
    assert!(GraphStore::open(root.path()).is_err());
    assert_eq!(
        std::fs::metadata(&path).unwrap().len(),
        MAX_GRAPH_BYTES as u64 + 1
    );
    let duplicate = serde_json::to_vec(&json!({
        "first":minimal_graph(), "second":minimal_graph()
    }))
    .unwrap();
    hagency_store::private::replace(&path, &duplicate).unwrap();
    assert!(GraphStore::open(root.path()).is_err());
    assert_eq!(std::fs::read(path).unwrap(), duplicate);
}

#[cfg(unix)]
#[test]
fn public_or_symlinked_documents_are_refused() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let external = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let path = root.path().join(GRAPHS_FILE);
    hagency_store::private::replace(&path, b"{}").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(GraphStore::open(root.path()).is_err());
    std::fs::remove_file(&path).unwrap();
    let target = external.path().join("private-graphs");
    hagency_store::private::replace(&target, b"{}").unwrap();
    symlink(&target, &path).unwrap();
    assert!(GraphStore::open(root.path()).is_err());
    assert_eq!(std::fs::read(target).unwrap(), b"{}");
}

#[test]
fn exhausted_counter_refuses_dispatch_without_wrapping_or_erasing_graph() {
    let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    hagency_store::private::replace(
        &root.path().join(COUNTER_FILE),
        u64::MAX.to_string().as_bytes(),
    )
    .unwrap();
    let store = GraphStore::open(root.path()).unwrap();
    store.create(&minimal_graph()).unwrap();
    let saved_graphs = std::fs::read(root.path().join(GRAPHS_FILE)).unwrap();
    assert_eq!(
        store.advance("retained").unwrap_err().code,
        "graph_dispatch_failed"
    );
    assert_eq!(
        std::fs::read(root.path().join(GRAPHS_FILE)).unwrap(),
        saved_graphs
    );
    assert!(!root.path().join(MESSAGES_FILE).exists());
    assert_eq!(
        std::fs::read_to_string(root.path().join(COUNTER_FILE)).unwrap(),
        u64::MAX.to_string()
    );
}

/// Exercise the extracted domain without constructing the console or an HTTP
/// listener. Original messages and dispatch identities survive reopening, and
/// advancing a saved graph cannot enqueue the same node twice.
#[test]
fn direct_domain_preserves_messages_dependencies_and_cancellation_after_reopen() {
    let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let old_message = json!({"id":"msg_0042", "summary":"Retained unrelated message"});
    hagency_store::private::replace(
        &root.path().join(MESSAGES_FILE),
        serde_json::to_vec(&json!([old_message.clone()]))
            .unwrap()
            .as_slice(),
    )
    .unwrap();
    hagency_store::private::replace(&root.path().join(COUNTER_FILE), b"3").unwrap();
    let store = GraphStore::open(root.path()).unwrap();
    store
        .create(&json!({
            "id":"original_graph", "owner":"operator", "label":"Conditional work",
            "nodes":{
                "a":{"assignee":"alpha", "description":"Inspect"},
                "b":{"assignee":"beta", "description":"Apply", "depends_on":["a"],
                    "condition":{"dep":"a", "path":"approved", "eq":true}},
                "c":{"assignee":"gamma", "description":"Alternative", "depends_on":["a"],
                    "condition":{"dep":"a", "path":"approved", "eq":false}}
            }
        }))
        .unwrap();
    let first = store.advance("original_graph").unwrap().unwrap();
    assert_eq!(first.nodes["a"].message_id.as_deref(), Some("msg_0043"));
    assert_eq!(first.nodes["b"].status, "pending");
    assert_eq!(first.nodes["c"].status, "pending");
    let saved_graphs = std::fs::read(root.path().join(GRAPHS_FILE)).unwrap();
    assert!(
        saved_graphs.len() > 512,
        "Regression must exceed the old secret-reader bound"
    );
    let saved_messages = std::fs::read(root.path().join(MESSAGES_FILE)).unwrap();
    drop(store);

    let store = GraphStore::open(root.path()).unwrap();
    store.advance("original_graph").unwrap();
    assert_eq!(
        std::fs::read(root.path().join(GRAPHS_FILE)).unwrap(),
        saved_graphs
    );
    assert_eq!(
        std::fs::read(root.path().join(MESSAGES_FILE)).unwrap(),
        saved_messages
    );
    store
        .update_node(
            "original_graph",
            "a",
            &json!({
                "status":"complete", "result":{"approved":true, "report":"original result"}
            }),
        )
        .unwrap();
    let advanced = store.advance("original_graph").unwrap().unwrap();
    assert_eq!(advanced.nodes["b"].status, "dispatched");
    assert_eq!(advanced.nodes["b"].message_id.as_deref(), Some("msg_0044"));
    assert_eq!(advanced.nodes["c"].status, "skipped");
    let messages = read_json(&root.path().join(MESSAGES_FILE), MAX_MESSAGES_BYTES)
        .unwrap()
        .unwrap();
    let rows = messages.as_array().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0], old_message);
    assert_eq!(rows[2]["to"], "beta");
    assert_eq!(
        rows[2]["schema"]["payload"]["dispatchKey"],
        "task_graph_dispatch:original_graph:b"
    );
    assert_eq!(
        rows[2]["schema"]["payload"]["dependencyResults"][0]["result"]["report"],
        "original result"
    );

    let cancelled = store.delete("original_graph").unwrap().unwrap();
    assert_eq!(cancelled.status, "cancelled");
    assert_eq!(cancelled.nodes["a"].status, "complete");
    assert_eq!(cancelled.nodes["b"].status, "cancelled");
    assert_eq!(cancelled.nodes["c"].status, "skipped");
    let saved_graphs = std::fs::read(root.path().join(GRAPHS_FILE)).unwrap();
    let saved_messages = std::fs::read(root.path().join(MESSAGES_FILE)).unwrap();
    drop(store);
    let reopened = GraphStore::open(root.path()).unwrap();
    assert_eq!(reopened.list(Some("cancelled")).len(), 1);
    reopened.advance("original_graph").unwrap();
    reopened.delete("original_graph").unwrap();
    assert_eq!(
        std::fs::read(root.path().join(GRAPHS_FILE)).unwrap(),
        saved_graphs
    );
    assert_eq!(
        std::fs::read(root.path().join(MESSAGES_FILE)).unwrap(),
        saved_messages
    );
}
