use super::*;

struct Fixture {
    temp: tempfile::TempDir,
    registry: OwnerResourceRegistry,
    profile: ProfileAuthority,
}
impl Fixture {
    async fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let registry = OwnerResourceRegistry::open_root(&temp.path().join("registry")).unwrap();
        let profile = profile('a', "@owner:example.test");
        registry
            .transact_operator_graph(&profile, command("create", 0), |_, writer| {
                writer.enqueue("node", |id| {
                    Ok(format!("original message {id}").into_bytes())
                })?;
                Ok(b"original graph document".to_vec())
            })
            .await
            .unwrap();
        Self {
            temp,
            registry,
            profile,
        }
    }
    async fn bind(&self, id: &str, revision: u64, bytes: Vec<u8>) -> GraphJournalReceipt {
        self.registry
            .transact_operator_graph(&self.profile, command(id, revision), move |old, writer| {
                writer.bind_recipient("node", bytes)?;
                Ok(old.unwrap().document)
            })
            .await
            .unwrap()
    }
    async fn current(&self) -> Option<GraphRecipientBinding> {
        self.registry
            .operator_graph_recipient(&self.profile, "scope", "graph", "node")
            .await
            .unwrap()
    }
    async fn outbox(&self) -> (String, Vec<u8>, String, String) {
        self.registry.0.store.call(4096, |db| {
            Ok(db.db.query_row("SELECT message_id,payload,payload_digest,state FROM owner_operator_graph_outbox", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?)
        }).await.unwrap()
    }
    async fn counts(&self) -> (u64, u64, u64, u64) {
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
                    db.db.query_row(
                        "SELECT COUNT(*) FROM owner_operator_graph_recipients",
                        [],
                        |r| r.get(0),
                    )?,
                ))
            })
            .await
            .unwrap()
    }
}
fn profile(ch: char, account: &str) -> ProfileAuthority {
    ProfileAuthority::new(
        ch.to_string().repeat(64),
        account.into(),
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
fn snapshot(who: &str) -> Vec<u8> {
    // Whitespace and field order are part of the exact native review bytes.
    format!("{{ \"recipient\":\"@{who}:example.test\", \"review\":{{\"fraction\":0.125,\"label\":\"工作\"}} }}").into_bytes()
}

#[tokio::test]
async fn binding_replacement_restarts_and_replays_original_receipt_without_restoring_old_target() {
    let mut f = Fixture::new().await;
    let original_outbox = f.outbox().await;
    let original = snapshot("original");
    let first = f.bind("bind", 1, original.clone()).await;
    assert_eq!(
        (
            first.revision,
            first.queued_messages,
            first.suppressed_messages
        ),
        (2, 0, 0)
    );
    assert_eq!(
        f.current().await.unwrap(),
        GraphRecipientBinding {
            snapshot: original.clone(),
            snapshot_digest: bytes_digest(&original),
            revision: 2
        }
    );
    let replacement = snapshot("replacement");
    f.bind("replace", 2, replacement.clone()).await;
    assert_eq!(f.outbox().await, original_outbox);
    assert_eq!(f.counts().await, (1, 3, 1, 1));
    f.registry
        .transact_operator_graph(&f.profile, command("cancel", 3), |old, w| {
            w.suppress_pending()?;
            Ok(old.unwrap().document)
        })
        .await
        .unwrap();
    f.registry
        .transact_operator_graph(&f.profile, command("reopen", 4), |old, _| {
            Ok(old.unwrap().document)
        })
        .await
        .unwrap();
    f.registry.shutdown().await.unwrap();
    f.registry = OwnerResourceRegistry::open_root(&f.temp.path().join("registry")).unwrap();
    assert_eq!(
        f.registry
            .operator_graph_command_receipt(&f.profile, command("bind", 1))
            .await
            .unwrap(),
        Some(first.clone())
    );
    assert_eq!(
        f.registry
            .transact_operator_graph(&f.profile, command("bind", 1), |_, _| panic!(
                "original replay must not bind or observe again"
            ))
            .await
            .unwrap(),
        first
    );
    assert_eq!(
        f.current().await.unwrap(),
        GraphRecipientBinding {
            snapshot: replacement.clone(),
            snapshot_digest: bytes_digest(&replacement),
            revision: 3
        }
    );
    let final_outbox = f.outbox().await;
    assert_eq!(
        (&final_outbox.0, &final_outbox.1, &final_outbox.2),
        (&original_outbox.0, &original_outbox.1, &original_outbox.2)
    );
    assert_eq!(final_outbox.3, "suppressed");
    assert_eq!(
        f.registry
            .operator_graph_record(&f.profile, "scope", "graph")
            .await
            .unwrap()
            .unwrap()
            .document,
        b"original graph document"
    );
    f.registry
        .0
        .store
        .call(1024, |db| {
            for table in [
                "task_graphs",
                "operator_messages",
                "runner_dispatches",
                "task_outbox",
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
async fn binding_cas_and_exact_command_receipt_lookup_reject_changed_payload_or_scope() {
    let f = Fixture::new().await;
    assert!(
        f.registry
            .operator_graph_command_receipt(&f.profile, command("bind", 1))
            .await
            .unwrap()
            .is_none()
    );
    f.bind("bind", 1, snapshot("original")).await;
    for change in 0..4 {
        let mut c = command("bind", 1);
        match change {
            0 => c.input_digest = "b".repeat(64),
            1 => c.scope_id = "other".into(),
            2 => c.graph_id = "other".into(),
            _ => c.expected_revision = 2,
        }
        assert!(matches!(
            f.registry
                .operator_graph_command_receipt(&f.profile, c)
                .await,
            Err(Error::Conflict)
        ));
    }
    assert!(matches!(
        f.registry
            .transact_operator_graph(&f.profile, command("stale", 1), |_, _| panic!(
                "CAS checked first"
            ))
            .await,
        Err(Error::Conflict)
    ));
    assert_eq!(f.current().await.unwrap().snapshot, snapshot("original"));
    assert_eq!(f.counts().await, (1, 2, 1, 1));
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn binding_and_graph_and_receipt_roll_back_together_on_callback_sqlite_or_retirement_failure()
{
    for fault in 0..3 {
        let f = Fixture::new().await;
        f.bind("first", 1, snapshot("original")).await;
        let original_outbox = f.outbox().await;
        if fault == 1 {
            f.registry.0.store.call(1024,|db| {
                db.db.execute_batch("CREATE TEMP TRIGGER reject_binding_receipt BEFORE INSERT ON owner_operator_graph_commands BEGIN SELECT RAISE(ABORT,'injected receipt failure'); END;")?;
                Ok(())
            }).await.unwrap();
        }
        let retire = f.profile.clone();
        let result = f
            .registry
            .transact_operator_graph(&f.profile, command("failed", 2), move |old, w| {
                w.bind_recipient("node", snapshot("replacement"))?;
                w.enqueue("new_node", |_| Ok(b"must rollback".to_vec()))?;
                if fault == 0 {
                    return Err(Error::State);
                }
                if fault == 2 {
                    retire.retire();
                }
                Ok(old.unwrap().document)
            })
            .await;
        match fault {
            0 => assert!(matches!(result, Err(Error::State))),
            1 => assert!(matches!(result, Err(Error::Sqlite(_)))),
            _ => assert!(matches!(result, Err(Error::LocalAuthority))),
        }
        let renewed = profile('a', "@owner:example.test");
        let current = f
            .registry
            .operator_graph_recipient(&renewed, "scope", "graph", "node")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (current.revision, current.snapshot),
            (2, snapshot("original"))
        );
        assert_eq!(
            f.registry
                .operator_graph_record(&renewed, "scope", "graph")
                .await
                .unwrap()
                .unwrap()
                .revision,
            2
        );
        assert!(
            f.registry
                .operator_graph_command_receipt(&renewed, command("failed", 2))
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(f.outbox().await, original_outbox);
        assert_eq!(f.counts().await, (1, 2, 1, 1));
        f.registry.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn binding_refuses_suppressed_or_future_issued_outbox_and_creation() {
    for issued in [false, true] {
        let f = Fixture::new().await;
        f.bind("first", 1, snapshot("original")).await;
        if issued {
            // Simulate a future dispatch state. New states must fail closed in
            // this writer, even if a later schema admits them.
            f.registry.0.store.call(1024,|db| {
                db.db.execute_batch("DROP TRIGGER owner_operator_graph_outbox_immutable; PRAGMA ignore_check_constraints=ON; UPDATE owner_operator_graph_outbox SET state='issued'; PRAGMA ignore_check_constraints=OFF;")?;
                Ok(())
            }).await.unwrap();
        } else {
            f.registry
                .transact_operator_graph(&f.profile, command("cancel", 2), |old, w| {
                    w.suppress_pending()?;
                    Ok(old.unwrap().document)
                })
                .await
                .unwrap();
        }
        let before = f.outbox().await;
        let revision = if issued { 2 } else { 3 };
        assert!(matches!(
            f.registry
                .transact_operator_graph(&f.profile, command("refused", revision), |old, w| {
                    w.bind_recipient("node", snapshot("replacement"))?;
                    Ok(old.unwrap().document)
                })
                .await,
            Err(Error::Conflict)
        ));
        assert_eq!(f.current().await.unwrap().snapshot, snapshot("original"));
        assert_eq!(f.outbox().await, before);
        let mut create = command("unreviewed_create", 0);
        create.graph_id = "new_graph".into();
        assert!(matches!(
            f.registry
                .transact_operator_graph(&f.profile, create, |_, w| {
                    w.bind_recipient("node", snapshot("new"))?;
                    Ok(vec![])
                })
                .await,
            Err(Error::Conflict)
        ));
        f.registry.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn binding_validates_exact_json_size_and_poisoned_errors_cannot_commit() {
    let f = Fixture::new().await;
    for (index, bytes) in [
        vec![],
        b"[]".to_vec(),
        b"null".to_vec(),
        vec![b' '; MAX_SNAPSHOT + 1],
    ]
    .into_iter()
    .enumerate()
    {
        let result = f
            .registry
            .transact_operator_graph(
                &f.profile,
                command(&format!("bad_{index}"), 1),
                move |old, w| {
                    assert!(w.bind_recipient("node", bytes).is_err());
                    Ok(old.unwrap().document)
                },
            )
            .await;
        assert!(matches!(result, Err(Error::Unqualified)));
    }
    assert_eq!(f.counts().await, (1, 1, 1, 0));
    let bytes = format!("{{\"p\":\"{}\"}}", "x".repeat(MAX_SNAPSHOT - 8)).into_bytes();
    assert_eq!(bytes.len(), MAX_SNAPSHOT);
    f.bind("boundary", 1, bytes.clone()).await;
    assert_eq!(f.current().await.unwrap().snapshot, bytes);
    f.registry
        .0
        .store
        .call(1024, |db| {
            for sql in [
                "UPDATE owner_operator_graph_recipients SET node_id='changed'",
                "UPDATE owner_operator_graph_recipients SET snapshot=zeroblob(32769)",
                "UPDATE owner_operator_graph_recipients SET snapshot=x'7b7d'",
                "DELETE FROM owner_operator_graph_recipients",
            ] {
                assert!(db.db.execute(sql, []).is_err(), "must reject {sql}");
            }
            Ok(())
        })
        .await
        .unwrap();
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn binding_isolates_accounts_profiles_scopes_and_rejects_corrupt_digest() {
    let f = Fixture::new().await;
    f.bind("first", 1, snapshot("original")).await;
    for other in [
        profile('b', "@owner:example.test"),
        profile('a', "@other:example.test"),
    ] {
        assert!(
            f.registry
                .operator_graph_recipient(&other, "scope", "graph", "node")
                .await
                .unwrap()
                .is_none()
        );
    }
    for (scope, graph, node) in [
        ("other", "graph", "node"),
        ("scope", "other", "node"),
        ("scope", "graph", "other"),
    ] {
        assert!(
            f.registry
                .operator_graph_recipient(&f.profile, scope, graph, node)
                .await
                .unwrap()
                .is_none()
        );
    }
    f.registry.0.store.call(1024,|db| {
        db.db.execute_batch("DROP TRIGGER owner_operator_graph_recipients_update; UPDATE owner_operator_graph_recipients SET snapshot=x'7b7d';")?;
        Ok(())
    }).await.unwrap();
    assert!(matches!(
        f.registry
            .operator_graph_recipient(&f.profile, "scope", "graph", "node")
            .await,
        Err(Error::Quarantined)
    ));
    assert!(matches!(
        f.registry
            .transact_operator_graph(&f.profile, command("replace_corrupt", 2), |old, w| {
                w.bind_recipient("node", snapshot("new"))?;
                Ok(old.unwrap().document)
            })
            .await,
        Err(Error::Quarantined)
    ));
    f.profile.retire();
    assert!(matches!(
        f.registry
            .operator_graph_recipient(&f.profile, "scope", "graph", "node")
            .await,
        Err(Error::LocalAuthority)
    ));
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn binding_node_capacity_preserves_held_messages_and_allows_reviewed_replacement() {
    let f = Fixture::new().await;
    let before = f.outbox().await;
    f.registry
        .transact_operator_graph(&f.profile, command("all_nodes", 1), |old, w| {
            for n in 0..256 {
                w.bind_recipient(&format!("node{n}"), snapshot("original"))?;
            }
            Ok(old.unwrap().document)
        })
        .await
        .unwrap();
    assert!(matches!(
        f.registry
            .transact_operator_graph(&f.profile, command("overflow", 2), |old, w| {
                w.bind_recipient("overflow", snapshot("new"))?;
                Ok(old.unwrap().document)
            })
            .await,
        Err(Error::Capacity)
    ));
    f.registry
        .transact_operator_graph(&f.profile, command("replace_at_capacity", 2), |old, w| {
            w.bind_recipient("node0", snapshot("replacement"))?;
            w.bind_recipient("node0", snapshot("replacement"))?;
            Ok(old.unwrap().document)
        })
        .await
        .unwrap();
    assert_eq!(f.counts().await, (1, 3, 1, 256));
    assert_eq!(f.outbox().await, before);
    f.registry.0.store.call(1024,|db| {
        assert!(db.db.execute("INSERT INTO owner_operator_graph_recipients SELECT registry_id,profile,owner_mxid,scope_id,graph_id,'overflow',revision+1,x'7b7d',?1 FROM owner_operator_graphs",[bytes_digest(b"{}")]).is_err());
        Ok(())
    }).await.unwrap();
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn schema97_upgrade_retains_graph_messages_receipts_and_adds_no_binding() {
    let mut f = Fixture::new().await;
    let id = f.registry.id().to_owned();
    let before = f.outbox().await;
    f.registry.shutdown().await.unwrap();
    let db = Connection::open(f.temp.path().join("registry/domain.sqlite3")).unwrap();
    db.execute_batch("DROP TABLE IF EXISTS owner_shared_native_checkpoints;DROP TABLE IF EXISTS owner_shared_native_turns;DROP TABLE IF EXISTS owner_shared_unstarted_runtimes;DROP TABLE IF EXISTS owner_shared_decrypted_inputs;DROP TABLE IF EXISTS owner_web_catalog_settings;DROP TABLE owner_operator_graph_recipients; DROP TRIGGER IF EXISTS owner_resource_amended_ceiling_retained;DROP TABLE IF EXISTS owner_resource_ceiling_amendments;DROP TABLE IF EXISTS owner_shared_unstarted_attempts;DROP TABLE IF EXISTS owner_shared_budget_demand_terminals;DROP TABLE IF EXISTS owner_shared_tool_policies;PRAGMA user_version=97;")
        .unwrap();
    drop(db);
    f.registry = OwnerResourceRegistry::open_root(&f.temp.path().join("registry")).unwrap();
    assert_eq!(f.registry.id(), id);
    assert_eq!(f.outbox().await, before);
    assert!(f.current().await.is_none());
    assert_eq!(f.counts().await, (1, 1, 1, 0));
    assert!(
        f.registry
            .operator_graph_command_receipt(&f.profile, command("create", 0))
            .await
            .unwrap()
            .is_some()
    );
    f.registry
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
    f.registry.shutdown().await.unwrap();
}
