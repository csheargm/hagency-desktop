use super::*;

struct Fixture {
    temp: tempfile::TempDir,
    registry: OwnerResourceRegistry,
    profile: ProfileAuthority,
    live: Arc<AtomicBool>,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let registry = OwnerResourceRegistry::open_root(&temp.path().join("registry")).unwrap();
        Self {
            temp,
            registry,
            profile: profile('a', "@owner:example.test"),
            live: Arc::new(AtomicBool::new(true)),
        }
    }
    fn prepare(&self, id: &str, docs: &[Option<Vec<u8>>; 3]) -> PreparedOperatorGraphArchive {
        self.prepare_with(id, docs, "b".repeat(64), self.profile.clone())
    }
    fn prepare_with(
        &self,
        id: &str,
        docs: &[Option<Vec<u8>>; 3],
        binding: String,
        profile: ProfileAuthority,
    ) -> PreparedOperatorGraphArchive {
        let live = self.live.clone();
        self.registry
            .prepare_operator_graph_archive(
                profile,
                id.into(),
                binding,
                summary(docs),
                std::array::from_fn(|i| docs[i].as_ref().map(|v| v.len() as u64)),
                Instant::now() + Duration::from_secs(50),
                move || live.load(Ordering::Acquire),
                || true,
            )
            .unwrap()
    }
}
fn profile(ch: char, owner: &str) -> ProfileAuthority {
    ProfileAuthority::new(
        ch.to_string().repeat(64),
        owner.into(),
        Instant::now() + Duration::from_secs(120),
    )
    .unwrap()
}
fn summary(docs: &[Option<Vec<u8>>; 3]) -> OperatorGraphHistorySummary {
    let mut digest = Sha256::new();
    digest.update(b"hagency.operator-graph-documents.v1\0");
    for ((name, _), bytes) in DOCUMENTS.iter().zip(docs) {
        digest.update(name.as_bytes());
        digest.update([0, u8::from(bytes.is_some())]);
        if let Some(bytes) = bytes {
            digest.update((bytes.len() as u64).to_be_bytes());
            digest.update(bytes);
        }
    }
    OperatorGraphHistorySummary {
        source_digest: format!("{:x}", digest.finalize()),
        present_documents: docs.iter().flatten().count(),
        graphs: 1,
        nodes: 1,
        messages: 1,
        dispatch_messages: 1,
        orphan_dispatch_messages: 0,
        unresolved_principal_names: 2,
        original_counter: docs[2].as_ref().map(|_| "9007199254740993".into()),
        effective_counter: "9007199254740993".into(),
    }
}
fn documents() -> [Option<Vec<u8>>; 3] {
    [Some(br#"{ "original": {"owner":"Historical owner","unknown":{"preserve":true}} }"#.to_vec()),Some(br#"[{"id":"msg_9007199254740993","viewToken":"never-in-public-receipt","full":"Original result"}]"#.to_vec()),Some(b"9007199254740993\n".to_vec())]
}
async fn copy(p: &PreparedOperatorGraphArchive, docs: &[Option<Vec<u8>>; 3]) {
    p.begin().await.unwrap();
    for (document, bytes) in docs.iter().enumerate() {
        if let Some(bytes) = bytes {
            for (index, bytes) in bytes.chunks(GRAPH_ARCHIVE_CHUNK_BYTES).enumerate() {
                p.write_chunk(document, index, bytes.to_vec())
                    .await
                    .unwrap();
            }
        }
    }
}
async fn rows(f: &Fixture) -> (u64, u64, u64, u64) {
    f.registry
        .0
        .store
        .call(1024, |db| {
            Ok((
                db.db.query_row(
                    "SELECT COUNT(*) FROM owner_operator_graph_archives",
                    [],
                    |r| r.get(0),
                )?,
                db.db.query_row(
                    "SELECT COUNT(*) FROM owner_operator_graph_archive_chunks",
                    [],
                    |r| r.get(0),
                )?,
                db.db
                    .query_row("SELECT COUNT(*) FROM task_graphs", [], |r| r.get(0))?,
                db.db
                    .query_row("SELECT COUNT(*) FROM operator_messages", [], |r| r.get(0))?,
            ))
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn exact_documents_and_receipt_survive_restart_without_execution_or_secret_projection() {
    let mut f = Fixture::new();
    let docs = documents();
    let p = f.prepare("original", &docs);
    copy(&p, &docs).await;
    let receipt = p.seal().await.unwrap();
    assert_eq!(receipt.state, "archived");
    assert!(!receipt.runtime_delivery_authorized && !receipt.source_writer_fenced);
    let public = serde_json::to_string(&receipt).unwrap();
    for private in [
        "Historical owner",
        "Original result",
        "never-in-public-receipt",
    ] {
        assert!(!public.contains(private));
    }
    assert_eq!(rows(&f).await, (1, 3, 0, 0));
    drop(p);
    f.registry.shutdown().await.unwrap();
    f.registry = OwnerResourceRegistry::open_root(&f.temp.path().join("registry")).unwrap();
    assert_eq!(
        f.registry
            .operator_graph_archive_receipt(&f.profile, "original".into())
            .await
            .unwrap(),
        Some(receipt.clone())
    );
    for (index, bytes) in docs.iter().enumerate() {
        assert_eq!(
            f.registry
                .operator_graph_archive_chunk(&f.profile, "original".into(), index, 0)
                .await
                .unwrap(),
            *bytes.as_ref().unwrap()
        );
    }
    let resumed = f.prepare("original", &docs);
    copy(&resumed, &docs).await;
    assert_eq!(resumed.seal().await.unwrap(), receipt);
    assert_eq!(rows(&f).await, (1, 3, 0, 0));
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn interrupted_copy_is_invisible_and_resumes_same_command_after_restart() {
    let mut f = Fixture::new();
    let docs = documents();
    let p = f.prepare("partial", &docs);
    assert_eq!(p.begin().await.unwrap().state, "copying");
    p.write_chunk(0, 0, docs[0].clone().unwrap()).await.unwrap();
    assert!(matches!(p.seal().await, Err(Error::Unqualified)));
    assert!(matches!(
        f.registry
            .operator_graph_archive_chunk(&f.profile, "partial".into(), 0, 0)
            .await,
        Err(Error::Unqualified)
    ));
    drop(p);
    f.registry.shutdown().await.unwrap();
    f.registry = OwnerResourceRegistry::open_root(&f.temp.path().join("registry")).unwrap();
    assert_eq!(
        f.registry
            .operator_graph_archive_receipt(&f.profile, "partial".into())
            .await
            .unwrap()
            .unwrap()
            .state,
        "copying"
    );
    let p = f.prepare("partial", &docs);
    copy(&p, &docs).await;
    p.seal().await.unwrap();
    assert_eq!(rows(&f).await, (1, 3, 0, 0));
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn changed_command_payload_or_second_key_cannot_replace_original_history() {
    let f = Fixture::new();
    let docs = documents();
    let p = f.prepare("first", &docs);
    copy(&p, &docs).await;
    p.seal().await.unwrap();
    let mut changed = docs.clone();
    changed[1].as_mut().unwrap().push(b' ');
    assert!(matches!(
        f.prepare("first", &changed).begin().await,
        Err(Error::Conflict)
    ));
    assert!(matches!(
        f.prepare("second", &docs).begin().await,
        Err(Error::Conflict)
    ));
    assert!(matches!(
        f.prepare_with("first", &docs, "c".repeat(64), f.profile.clone())
            .begin()
            .await,
        Err(Error::Conflict)
    ));
    let mut bytes = docs[0].clone().unwrap();
    bytes[0] = b' ';
    assert!(matches!(
        p.write_chunk(0, 0, bytes).await,
        Err(Error::Conflict)
    ));
    assert_eq!(rows(&f).await, (1, 3, 0, 0));
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn other_account_profile_and_retired_authority_cannot_read_or_resume() {
    let f = Fixture::new();
    let docs = documents();
    let p = f.prepare("private", &docs);
    copy(&p, &docs).await;
    p.seal().await.unwrap();
    for other in [
        profile('b', "@owner:example.test"),
        profile('a', "@other:example.test"),
    ] {
        assert!(matches!(
            f.registry
                .operator_graph_archive_receipt(&other, "private".into())
                .await,
            Err(Error::LocalAuthority)
        ));
        assert!(matches!(
            f.registry
                .operator_graph_archive_chunk(&other, "private".into(), 0, 0)
                .await,
            Err(Error::LocalAuthority)
        ));
        assert!(matches!(
            f.prepare_with("private", &docs, "b".repeat(64), other)
                .begin()
                .await,
            Err(Error::LocalAuthority)
        ));
    }
    f.profile.retire();
    assert!(matches!(p.seal().await, Err(Error::LocalAuthority)));
    assert!(matches!(
        f.registry
            .operator_graph_archive_receipt(&f.profile, "private".into())
            .await,
        Err(Error::LocalAuthority)
    ));
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn source_revocation_at_writer_blocks_queued_chunk_and_keeps_copy_incomplete() {
    let f = Fixture::new();
    let docs = documents();
    let p = f.prepare("queued", &docs);
    p.begin().await.unwrap();
    let store = f.registry.0.store.clone();
    let (send, ready) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let blocker = tokio::spawn(async move {
        store
            .call(1024, move |_| {
                send.send(()).unwrap();
                blocked.recv_timeout(Duration::from_secs(10)).unwrap();
                Ok(())
            })
            .await
            .unwrap();
    });
    ready.await.unwrap();
    let mut pending = Box::pin(p.write_chunk(0, 0, docs[0].clone().unwrap()));
    tokio::select! {biased;v=&mut pending=>panic!("bypassed writer: {v:?}"),_ = tokio::task::yield_now()=>{}}
    f.live.store(false, Ordering::Release);
    release.send(()).unwrap();
    blocker.await.unwrap();
    assert!(matches!(pending.await, Err(Error::LocalAuthority)));
    assert_eq!(rows(&f).await, (1, 0, 0, 0));
    assert_eq!(
        f.registry
            .operator_graph_archive_receipt(&f.profile, "queued".into())
            .await
            .unwrap()
            .unwrap()
            .state,
        "copying"
    );
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn sealing_failure_rolls_back_without_publishing_partial_history() {
    let f = Fixture::new();
    let docs = documents();
    let p = f.prepare("fail-seal", &docs);
    copy(&p, &docs).await;
    f.registry.0.store.call(1024,|db|{db.db.execute_batch("CREATE TRIGGER fixture_fail_seal BEFORE UPDATE ON owner_operator_graph_archives BEGIN SELECT RAISE(ABORT,'fixture'); END;")?;Ok(())}).await.unwrap();
    assert!(p.seal().await.is_err());
    assert_eq!(
        f.registry
            .operator_graph_archive_receipt(&f.profile, "fail-seal".into())
            .await
            .unwrap()
            .unwrap()
            .state,
        "copying"
    );
    assert!(
        f.registry
            .operator_graph_archive_chunk(&f.profile, "fail-seal".into(), 0, 0)
            .await
            .is_err()
    );
    f.registry
        .0
        .store
        .call(1024, |db| {
            db.db.execute_batch("DROP TRIGGER fixture_fail_seal;")?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(p.seal().await.unwrap().state, "archived");
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn incorrect_byte_digest_cannot_be_sealed_even_with_complete_chunk_count() {
    let f = Fixture::new();
    let docs = documents();
    let p = f.prepare("checksum", &docs);
    p.begin().await.unwrap();
    let mut corrupt = docs.clone();
    corrupt[0].as_mut().unwrap()[0] = b' ';
    copy(&p, &corrupt).await;
    assert!(matches!(p.seal().await, Err(Error::Quarantined)));
    assert_eq!(
        f.registry
            .operator_graph_archive_receipt(&f.profile, "checksum".into())
            .await
            .unwrap()
            .unwrap()
            .state,
        "copying"
    );
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn changed_source_bytes_block_final_publish_without_rehashing_source_on_every_chunk() {
    let f=Fixture::new();let docs=documents();
    let contents=Arc::new(AtomicBool::new(true));let contents_gate=contents.clone();
    let observations=Arc::new(std::sync::atomic::AtomicUsize::new(0));let observed=observations.clone();
    let p=f.registry.prepare_operator_graph_archive(
        f.profile.clone(),"content-check".into(),"b".repeat(64),summary(&docs),
        std::array::from_fn(|i|docs[i].as_ref().map(|v|v.len() as u64)),
        Instant::now()+Duration::from_secs(50),||true,
        move || {observed.fetch_add(1,Ordering::AcqRel);contents_gate.load(Ordering::Acquire)},
    ).unwrap();
    copy(&p,&docs).await;
    assert_eq!(observations.load(Ordering::Acquire),0);
    contents.store(false,Ordering::Release);
    assert!(matches!(p.seal().await,Err(Error::LocalAuthority)));
    assert_eq!(observations.load(Ordering::Acquire),1);
    assert_eq!(f.registry.operator_graph_archive_receipt(&f.profile,"content-check".into()).await.unwrap().unwrap().state,"copying");
    assert!(f.registry.operator_graph_archive_chunk(&f.profile,"content-check".into(),0,0).await.is_err());
    contents.store(true,Ordering::Release);
    assert_eq!(p.seal().await.unwrap().state,"archived");
    assert_eq!(observations.load(Ordering::Acquire),2);
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn archive_sql_prevents_rewrites_deletes_and_new_chunks_after_sealing() {
    let f = Fixture::new();
    let docs = documents();
    let p = f.prepare("immutable", &docs);
    copy(&p, &docs).await;
    p.seal().await.unwrap();
    f.registry
        .0
        .store
        .call(4096, |db| {
            for sql in [
                "UPDATE owner_operator_graph_archives SET state='copying'",
                "UPDATE owner_operator_graph_archives SET owner_mxid='@other:test'",
                "DELETE FROM owner_operator_graph_archives",
                "UPDATE owner_operator_graph_archive_chunks SET bytes=X'41'",
                "DELETE FROM owner_operator_graph_archive_chunks",
                "INSERT INTO owner_operator_graph_archive_chunks VALUES('immutable',1,1,X'41')",
            ] {
                assert!(db.db.execute_batch(sql).is_err(), "{sql}");
            }
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(rows(&f).await, (1, 3, 0, 0));
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn absent_and_empty_documents_stay_distinct_and_out_of_range_chunks_are_refused() {
    let f = Fixture::new();
    for (index, docs) in [[None, None, None], [Some(vec![]), None, None]]
        .into_iter()
        .enumerate()
    {
        let p = f.prepare_with(
            &format!("shape{index}"),
            &docs,
            if index == 0 {
                "b".repeat(64)
            } else {
                "c".repeat(64)
            },
            f.profile.clone(),
        );
        p.begin().await.unwrap();
        assert!(matches!(
            p.write_chunk(0, 0, vec![]).await,
            Err(Error::Unqualified)
        ));
        assert!(matches!(
            p.write_chunk(3, 0, vec![1]).await,
            Err(Error::Unqualified)
        ));
        p.seal().await.unwrap();
    }
    let a = f
        .registry
        .operator_graph_archive_receipt(&f.profile, "shape0".into())
        .await
        .unwrap()
        .unwrap();
    let b = f
        .registry
        .operator_graph_archive_receipt(&f.profile, "shape1".into())
        .await
        .unwrap()
        .unwrap();
    assert_ne!(a.history.source_digest, b.history.source_digest);
    assert_eq!(rows(&f).await, (2, 0, 0, 0));
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn full_64_mib_messages_use_bounded_chunks_and_preserve_last_byte() {
    let f = Fixture::new();
    let mut messages = vec![b' '; 64 * 1024 * 1024];
    messages[0] = b'[';
    *messages.last_mut().unwrap() = b']';
    let docs = [None, Some(messages), None];
    let p = f.prepare("large", &docs);
    copy(&p, &docs).await;
    p.seal().await.unwrap();
    assert_eq!(rows(&f).await, (1, 64, 0, 0));
    let last = f
        .registry
        .operator_graph_archive_chunk(&f.profile, "large".into(), 1, 63)
        .await
        .unwrap();
    assert_eq!(last.len(), GRAPH_ARCHIVE_CHUNK_BYTES);
    assert_eq!(last.last(), Some(&b']'));
    assert!(matches!(
        p.write_chunk(1, 64, vec![1]).await,
        Err(Error::Unqualified)
    ));
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn schema95_upgrade_keeps_original_registry_identity_and_does_not_import_implicitly() {
    let f = Fixture::new();
    let id = f.registry.id().to_string();
    f.registry.shutdown().await.unwrap();
    let path = f.temp.path().join("registry/domain.sqlite3");
    let db = Connection::open(&path).unwrap();
    db.execute_batch("DROP TABLE IF EXISTS owner_shared_native_checkpoints;DROP TABLE IF EXISTS owner_shared_native_turns;DROP TABLE IF EXISTS owner_shared_unstarted_runtimes;DROP TABLE IF EXISTS owner_shared_decrypted_inputs;DROP TABLE IF EXISTS owner_web_catalog_settings;DROP TABLE IF EXISTS owner_operator_graph_recipients; DROP TABLE owner_operator_graph_outbox; DROP TABLE owner_operator_graph_commands; DROP TABLE owner_operator_graphs; DROP TABLE owner_operator_graph_archive_chunks; DROP TABLE owner_operator_graph_archives; DROP TRIGGER IF EXISTS owner_resource_amended_ceiling_retained;DROP TABLE IF EXISTS owner_resource_ceiling_amendments;DROP TABLE IF EXISTS owner_shared_unstarted_attempts;DROP TABLE IF EXISTS owner_shared_budget_demand_terminals;DROP TABLE IF EXISTS owner_shared_tool_policies;PRAGMA user_version=95;").unwrap();
    drop(db);
    let registry = OwnerResourceRegistry::open_root(&f.temp.path().join("registry")).unwrap();
    assert_eq!(registry.id(), id);
    assert!(
        registry
            .operator_graph_archive_receipt(&f.profile, "absent".into())
            .await
            .unwrap()
            .is_none()
    );
    registry
        .0
        .store
        .call(1024, |db| {
            assert_eq!(
                db.db
                    .pragma_query_value(None, "user_version", |r| r.get::<_, i32>(0))?,
                DOMAIN_SCHEMA_VERSION
            );
            Ok(())
        })
        .await
        .unwrap();
    registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn graph_reader_receipts_filter_profile_and_documents_require_seal() {
    let f=Fixture::new(); let docs=documents(); let p=f.prepare("original",&docs);
    p.begin().await.unwrap();
    let list=f.registry.operator_graph_archives(&f.profile).await.unwrap();
    assert_eq!(list.len(),1); assert_eq!(list[0].state,"copying");
    assert!(f.registry.operator_graph_history_document(&f.profile,"original".into()).await.is_err());
    assert!(f.registry.operator_graph_archives(&profile('c',"@other:example.test")).await.unwrap().is_empty());
    copy(&p,&docs).await; p.seal().await.unwrap();
    let bytes=f.registry.operator_graph_history_document(&f.profile,"original".into()).await.unwrap();
    assert_eq!(bytes,docs[0]);
    assert!(!String::from_utf8(bytes.unwrap()).unwrap().contains("never-in-public-receipt"));
    assert!(f.registry.operator_graph_history_document(&profile('a',"@other:example.test"),"original".into()).await.is_err());
    let list=f.registry.operator_graph_archives(&f.profile).await.unwrap();assert_eq!(list[0].state,"archived");
    f.registry.shutdown().await.unwrap();
    assert!(f.registry.operator_graph_archives(&f.profile).await.is_err());
}
#[tokio::test]
async fn graph_reader_absent_document_is_distinct_and_full_four_mib_is_bounded() {
    for present in [false,true] {
        let f=Fixture::new();
        let docs=[present.then(||vec![b' ';4*1024*1024]),None,None];
        let p=f.prepare("original",&docs);copy(&p,&docs).await;p.seal().await.unwrap();
        assert_eq!(f.registry.operator_graph_history_document(&f.profile,"original".into()).await.unwrap(),docs[0]);
        f.registry.shutdown().await.unwrap();
    }
}
