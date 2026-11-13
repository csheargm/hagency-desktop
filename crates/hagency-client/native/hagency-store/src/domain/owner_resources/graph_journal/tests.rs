use super::*;

struct Fixture {
    temp: tempfile::TempDir,
    registry: OwnerResourceRegistry,
    profile: ProfileAuthority,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let registry = OwnerResourceRegistry::open_root(&temp.path().join("registry")).unwrap();
        Self {
            temp,
            registry,
            profile: profile('a', "@owner:example.test"),
        }
    }
    async fn rows(&self) -> (u64, u64, u64) {
        self.registry
            .0
            .store
            .call(1024, |db| {
                Ok((
                    db.db
                        .query_row("SELECT COUNT(*) FROM owner_operator_graphs", [], |r| {
                            r.get(0)
                        })?,
                    db.db.query_row(
                        "SELECT COUNT(*) FROM owner_operator_graph_commands",
                        [],
                        |r| r.get(0),
                    )?,
                    db.db.query_row(
                        "SELECT COUNT(*) FROM owner_operator_graph_outbox",
                        [],
                        |r| r.get(0),
                    )?,
                ))
            })
            .await
            .unwrap()
    }
    async fn create(&self) -> GraphJournalReceipt {
        self.registry
            .transact_operator_graph(&self.profile, command("create", 0), |old, w| {
                assert!(old.is_none());
                w.enqueue("node", |id| {
                    Ok(format!("original private message {id}").into_bytes())
                })?;
                Ok(b"original graph".to_vec())
            })
            .await
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
fn command(id: &str, revision: u64) -> GraphJournalCommand {
    GraphJournalCommand {
        command_id: id.into(),
        scope_id: "scope".into(),
        graph_id: "graph".into(),
        expected_revision: revision,
        input_digest: bytes_digest(id.as_bytes()),
    }
}

#[tokio::test]
async fn journal_replay_after_cancel_and_restart_returns_original_without_callback() {
    let mut f = Fixture::new();
    let original = f.create().await;
    assert_eq!(
        (
            original.revision,
            original.queued_messages,
            original.suppressed_messages
        ),
        (1, 1, 0)
    );
    let cancelled = f
        .registry
        .transact_operator_graph(&f.profile, command("cancel", 1), |old, w| {
            assert_eq!(old.unwrap().document, b"original graph");
            w.suppress_pending()?;
            Ok(b"cancelled graph".to_vec())
        })
        .await
        .unwrap();
    assert_eq!(
        (
            cancelled.revision,
            cancelled.queued_messages,
            cancelled.suppressed_messages
        ),
        (2, 0, 1)
    );
    f.registry.shutdown().await.unwrap();
    f.registry = OwnerResourceRegistry::open_root(&f.temp.path().join("registry")).unwrap();
    let replay = f
        .registry
        .transact_operator_graph(&f.profile, command("create", 0), |_, _| {
            panic!("must replay before CAS")
        })
        .await
        .unwrap();
    assert_eq!(replay, original);
    let reopened = f
        .registry
        .transact_operator_graph(&f.profile, command("reopen", 2), |old, w| {
            assert_eq!(old.unwrap().revision, 2);
            assert_eq!(
                w.enqueue("node", |_| panic!(
                    "suppressed node retains original message"
                ))?
                .len(),
                64
            );
            Ok(b"reopened graph".to_vec())
        })
        .await
        .unwrap();
    assert_eq!(reopened.queued_messages, 0);
    assert_eq!(
        f.registry
            .transact_operator_graph(&f.profile, command("cancel", 1), |_, _| panic!(
                "replayed cancel"
            ))
            .await
            .unwrap(),
        cancelled
    );
    assert_eq!(
        f.registry
            .operator_graph_record(&f.profile, "scope", "graph")
            .await
            .unwrap()
            .unwrap(),
        GraphJournalRecord {
            revision: 3,
            document: b"reopened graph".to_vec()
        }
    );
    let outbox = f
        .registry
        .0
        .store
        .call(1024, |db| {
            Ok(db.db.query_row(
                "SELECT message_id,payload,state FROM owner_operator_graph_outbox",
                [],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Vec<u8>>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )?)
        })
        .await
        .unwrap();
    assert_eq!(
        outbox.1,
        format!("original private message {}", outbox.0).as_bytes()
    );
    assert_eq!(outbox.2, "suppressed");
    assert_eq!(f.rows().await, (1, 3, 1));
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn journal_rejects_changed_command_content_scope_graph_and_revision_before_callback() {
    let f = Fixture::new();
    f.create().await;
    for mutation in 0..4 {
        let mut c = command("create", 0);
        match mutation {
            0 => c.input_digest = "f".repeat(64),
            1 => c.scope_id = "other".into(),
            2 => c.graph_id = "other".into(),
            _ => c.expected_revision = 1,
        }
        assert!(matches!(
            f.registry
                .transact_operator_graph(&f.profile, c, |_, _| panic!("conflict must not execute"))
                .await,
            Err(Error::Conflict)
        ));
    }
    assert!(matches!(
        f.registry
            .transact_operator_graph(&f.profile, command("stale", 0), |_, _| panic!(
                "stale revision"
            ))
            .await,
        Err(Error::Conflict)
    ));
    assert_eq!(f.rows().await, (1, 1, 1));
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn journal_isolates_profile_owner_and_scope_and_preserves_unicode_ids() {
    let f = Fixture::new();
    f.create().await;
    for other in [
        profile('b', "@other:example.test"),
        profile('a', "@other:example.test"),
    ] {
        assert!(
            f.registry
                .operator_graph_record(&other, "scope", "graph")
                .await
                .unwrap()
                .is_none()
        );
    }
    assert!(
        f.registry
            .operator_graph_record(&f.profile, "another", "graph")
            .await
            .unwrap()
            .is_none()
    );
    let other = profile('b', "@other:example.test");
    f.registry
        .transact_operator_graph(&other, command("create", 0), |old, w| {
            assert!(old.is_none());
            w.enqueue("node", |_| Ok(b"other".to_vec()))?;
            Ok(b"other".to_vec())
        })
        .await
        .unwrap();
    let graph = "图".repeat(255);
    let node = "节".repeat(255);
    let mut c = command("unicode", 0);
    c.graph_id = graph.clone();
    f.registry
        .transact_operator_graph(&f.profile, c, move |_, w| {
            w.enqueue(&node, |_| Ok(vec![]))?;
            Ok(vec![])
        })
        .await
        .unwrap();
    assert_eq!(
        f.registry
            .operator_graph_record(&f.profile, "scope", &graph)
            .await
            .unwrap()
            .unwrap()
            .document,
        Vec::<u8>::new()
    );
    let unique = f
        .registry
        .0
        .store
        .call(1024, |db| {
            Ok(db.db.query_row(
                "SELECT COUNT(DISTINCT message_id) FROM owner_operator_graph_outbox",
                [],
                |r| r.get::<_, u64>(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(unique, 3);
    assert_eq!(f.rows().await, (3, 3, 3));
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn journal_callback_error_rolls_back_messages_graph_and_receipt() {
    let f = Fixture::new();
    assert!(matches!(
        f.registry
            .transact_operator_graph(&f.profile, command("failed", 0), |_, w| {
                w.enqueue("one", |_| Ok(b"private".to_vec()))?;
                Err(Error::State)
            })
            .await,
        Err(Error::State)
    ));
    assert_eq!(f.rows().await, (0, 0, 0));
    f.create().await;
    assert!(matches!(
        f.registry
            .transact_operator_graph(&f.profile, command("failed_cancel", 1), |_, w| {
                w.suppress_pending()?;
                Err(Error::State)
            })
            .await,
        Err(Error::State)
    ));
    let state = f
        .registry
        .0
        .store
        .call(1024, |db| {
            Ok(db
                .db
                .query_row("SELECT state FROM owner_operator_graph_outbox", [], |r| {
                    r.get::<_, String>(0)
                })?)
        })
        .await
        .unwrap();
    assert_eq!(state, "pending_binding");
    assert_eq!(f.rows().await, (1, 1, 1));
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn journal_caught_writer_error_poison_rolls_back_the_entire_transition() {
    let f = Fixture::new();
    let result = f
        .registry
        .transact_operator_graph(&f.profile, command("poison", 0), |_, w| {
            w.enqueue("first", |_| Ok(vec![1]))?;
            assert!(matches!(
                w.enqueue("second", |_| Err(Error::State)),
                Err(Error::State)
            ));
            Ok(vec![2])
        })
        .await;
    assert!(matches!(result, Err(Error::Unqualified)));
    assert_eq!(f.rows().await, (0, 0, 0));
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn journal_sqlite_receipt_rejection_rolls_back_graph_and_outbox() {
    let f = Fixture::new();
    f.registry.0.store.call(1024,|db|{db.db.execute_batch("CREATE TEMP TRIGGER reject_graph_receipt BEFORE INSERT ON owner_operator_graph_commands BEGIN SELECT RAISE(ABORT,'injected receipt failure'); END;")?;Ok(())}).await.unwrap();
    assert!(matches!(
        f.registry
            .transact_operator_graph(&f.profile, command("rejected", 0), |_, w| {
                w.enqueue("first", |_| Ok(vec![1]))?;
                Ok(vec![2])
            })
            .await,
        Err(Error::Sqlite(_))
    ));
    assert_eq!(f.rows().await, (0, 0, 0));
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn journal_authority_retired_inside_transition_or_payload_builder_rolls_back() {
    for in_builder in [false, true] {
        let f = Fixture::new();
        let retirement = f.profile.clone();
        assert!(matches!(
            f.registry
                .transact_operator_graph(&f.profile, command("retire", 0), move |_, w| {
                    w.enqueue("node", |_| {
                        if in_builder {
                            retirement.retire();
                        }
                        Ok(vec![1])
                    })?;
                    retirement.retire();
                    Ok(vec![2])
                })
                .await,
            Err(Error::LocalAuthority)
        ));
        assert_eq!(f.rows().await, (0, 0, 0));
        f.registry.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn journal_duplicate_enqueue_never_rebuilds_and_messages_cannot_be_rewritten() {
    let f = Fixture::new();
    let receipt = f
        .registry
        .transact_operator_graph(&f.profile, command("once", 0), |_, w| {
            let id = w.enqueue("node", |id| Ok(id.as_bytes().to_vec()))?;
            assert_eq!(w.enqueue("node", |_| panic!("duplicate builder"))?, id);
            Ok(vec![1])
        })
        .await
        .unwrap();
    assert_eq!(receipt.queued_messages, 1);
    f.registry
        .0
        .store
        .call(1024, |db| {
            for sql in [
                "UPDATE owner_operator_graph_outbox SET payload=x'01'",
                "UPDATE owner_operator_graph_outbox SET state='delivered'",
                "DELETE FROM owner_operator_graph_outbox",
                "UPDATE owner_operator_graph_commands SET receipt='{}'",
                "DELETE FROM owner_operator_graph_commands",
                "DELETE FROM owner_operator_graphs",
            ] {
                assert!(db.db.execute(sql, []).is_err(), "must reject {sql}");
            }
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(f.rows().await, (1, 1, 1));
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn journal_enforces_document_message_count_and_individual_payload_bounds() {
    let f = Fixture::new();
    assert!(matches!(
        f.registry
            .transact_operator_graph(&f.profile, command("oversize_graph", 0), |_, w| {
                w.enqueue("node", |_| Ok(vec![1]))?;
                Ok(vec![0; MAX_DOCUMENT + 1])
            })
            .await,
        Err(Error::Capacity)
    ));
    assert!(matches!(
        f.registry
            .transact_operator_graph(&f.profile, command("oversize_message", 0), |_, w| {
                w.enqueue("node", |_| Ok(vec![0; MAX_DOCUMENT + 1]))?;
                Ok(vec![])
            })
            .await,
        Err(Error::Capacity)
    ));
    assert!(matches!(
        f.registry
            .transact_operator_graph(&f.profile, command("too_many", 0), |_, w| {
                for n in 0..256 {
                    w.enqueue(&format!("node{n}"), |_| Ok(vec![]))?;
                }
                w.enqueue("overflow", |_| panic!("count checked before builder"))?;
                Ok(vec![])
            })
            .await,
        Err(Error::Capacity)
    ));
    assert_eq!(f.rows().await, (0, 0, 0));
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn journal_aggregate_bound_counts_suppressed_payloads_and_database_enforces_it() {
    let f = Fixture::new();
    f.registry
        .transact_operator_graph(&f.profile, command("empty", 0), |_, _| Ok(vec![]))
        .await
        .unwrap();
    let digest = bytes_digest(&vec![0; MAX_DOCUMENT]);
    f.registry.0.store.call(1024,move|db|{
        let tx=db.db.transaction()?;
        tx.execute("WITH RECURSIVE numbers(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM numbers WHERE n<16) INSERT INTO owner_operator_graph_outbox SELECT registry_id,profile,owner_mxid,scope_id,graph_id,CAST(n AS TEXT),printf('%064x',n),zeroblob(4194304),?1,'pending_binding' FROM owner_operator_graphs CROSS JOIN numbers",[digest])?;
        tx.execute("UPDATE owner_operator_graph_outbox SET state='suppressed'",[])?;
        assert!(tx.execute("INSERT INTO owner_operator_graph_outbox SELECT registry_id,profile,owner_mxid,scope_id,graph_id,'overflow',printf('%064x',17),x'01',printf('%064x',17),'pending_binding' FROM owner_operator_graphs",[]).is_err());
        tx.commit()?;Ok(())
    }).await.unwrap();
    assert!(matches!(
        f.registry
            .transact_operator_graph(&f.profile, command("too_large", 1), |_, w| {
                w.enqueue("additional", |_| Ok(vec![1]))?;
                Ok(vec![])
            })
            .await,
        Err(Error::Capacity)
    ));
    assert_eq!(f.rows().await, (1, 1, 16));
    assert_eq!(
        f.registry
            .operator_graph_record(&f.profile, "scope", "graph")
            .await
            .unwrap()
            .unwrap()
            .revision,
        1
    );
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn journal_retains_original_source_files_and_does_not_dispatch_runtime_work() {
    let f = Fixture::new();
    let originals = [
        ("task_graphs.json", b"original task graphs".as_slice()),
        ("messages.json", b"private original messages".as_slice()),
        (".msg_counter", b"9007199254740993".as_slice()),
    ];
    for (name, bytes) in originals {
        std::fs::write(f.temp.path().join(name), bytes).unwrap();
    }
    f.create().await;
    for (name, bytes) in originals {
        assert_eq!(std::fs::read(f.temp.path().join(name)).unwrap(), bytes);
    }
    f.registry
        .0
        .store
        .call(1024, |db| {
            for table in [
                "task_graphs",
                "operator_messages",
                "runner_dispatches",
                "task_outbox",
                "owner_operator_graph_archives",
            ] {
                assert_eq!(
                    db.db
                        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                            .get::<_, u64>(0))?,
                    0
                );
            }
            Ok(())
        })
        .await
        .unwrap();
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn journal_schema96_upgrade_preserves_registry_and_does_not_adopt_history() {
    let f = Fixture::new();
    let id = f.registry.id().to_owned();
    f.registry.shutdown().await.unwrap();
    let db = Connection::open(f.temp.path().join("registry/domain.sqlite3")).unwrap();
    db.execute_batch("DROP TABLE IF EXISTS owner_octos_model_resources;DROP TABLE IF EXISTS owner_octos_enrollments;DROP TABLE IF EXISTS owner_octos_profiles;DROP TABLE IF EXISTS owner_octos_key;DROP TABLE IF EXISTS owner_shared_native_checkpoints;DROP TABLE IF EXISTS owner_shared_native_turns;DROP TABLE IF EXISTS owner_shared_unstarted_runtimes;DROP TABLE IF EXISTS owner_shared_decrypted_inputs;DROP TABLE IF EXISTS owner_web_catalog_settings;DROP TABLE IF EXISTS owner_operator_graph_recipients; DROP TABLE owner_operator_graph_outbox; DROP TABLE owner_operator_graph_commands; DROP TABLE owner_operator_graphs; DROP TRIGGER IF EXISTS owner_resource_amended_ceiling_retained;DROP TABLE IF EXISTS owner_resource_ceiling_amendments;DROP TABLE IF EXISTS owner_shared_unstarted_attempts;DROP TABLE IF EXISTS owner_shared_budget_demand_terminals;DROP TABLE IF EXISTS owner_shared_tool_policies;PRAGMA user_version=96;").unwrap();
    drop(db);
    let registry = OwnerResourceRegistry::open_root(&f.temp.path().join("registry")).unwrap();
    assert_eq!(registry.id(), id);
    assert!(
        registry
            .operator_graph_record(&f.profile, "scope", "graph")
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
            assert_eq!(
                db.db.query_row(
                    "SELECT COUNT(*) FROM owner_operator_graph_commands",
                    [],
                    |r| r.get::<_, u64>(0)
                )?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    registry.shutdown().await.unwrap();
}
