use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

impl Manager {
    #[cfg(feature = "shared-intake")]
    pub(in crate::console::native) fn retained_stop_for_test(&self) -> bool {
        self.state.lock().unwrap().controls.iter().any(|t| !t.is_finished())
    }
    /// Test-only occupied admissions. No synthetic provider or canonical claim.
    pub(in crate::console::native) fn hold_capacity_for_test(
        &self,
        count: usize,
    ) -> Vec<oneshot::Sender<()>> {
        (0..count)
            .map(|n| {
                let (release, wait) = oneshot::channel();
                self.admit(
                    format!("held_{n}"),
                    "digest".into(),
                    "held".into(),
                    false,
                    move |slot, _, sender| async move {
                        slot.before_claim().map_err(|_| DrainFailure::Unknown)?;
                        let _ = wait.await;
                        let _ = sender.send(Err(unknown()));
                        Ok(())
                    },
                )
                .unwrap();
                release
            })
            .collect()
    }
    pub(in crate::console::native) async fn settle_capacity_for_test(
        &self,
    ) -> Result<(), DrainFailure> {
        self.custody.settle().await
    }
}

#[tokio::test]
async fn manager_capacity_bound_precedes_effect_and_same_key_is_retained() {
    let m = Manager::default();
    let holds = m.hold_capacity_for_test(MAX_PROCESSES);
    let effects = Arc::new(AtomicUsize::new(0));
    let e = effects.clone();
    assert!(
        m.admit(
            "overflow".into(),
            "d".into(),
            "a".into(),
            false,
            move |_, _, _| async move {
                e.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        )
        .is_err()
    );
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    assert!(m.running("held_0", "digest").is_err());
    assert!(
        m.admit(
            "held_0".into(),
            "digest".into(),
            "held".into(),
            false,
            |_, _, _| async { panic!("duplicate admission reached effect") }
        )
        .is_err()
    );
    assert!(m.running("held_0", "different").is_err());
    for release in holds {
        release.send(()).unwrap();
    }
    assert_eq!(m.settle_capacity_for_test().await, Ok(()));
    let rx = m
        .admit(
            "after".into(),
            "d".into(),
            "a".into(),
            false,
            |slot, _, sender| async move {
                slot.before_claim().unwrap();
                slot.issued(Some(1)).unwrap();
                let _ = sender.send(Err(unknown()));
                Ok(())
            },
        )
        .unwrap();
    assert!(rx.await.unwrap().is_err());
    assert_eq!(m.drain().await, Ok(()));
}

#[tokio::test]
async fn manager_capacity_waiter_cancel_keeps_original_job_and_close_waits() {
    let m = Manager::default();
    let (entered, claimed) = oneshot::channel();
    let (release, wait) = oneshot::channel();
    let rx = m
        .admit(
            "one".into(),
            "d".into(),
            "a".into(),
            false,
            |slot, mut cancel, sender| async move {
                slot.before_claim().unwrap();
                slot.issued(Some(3)).unwrap();
                entered.send(()).unwrap();
                cancel.changed().await.unwrap();
                assert!(*cancel.borrow());
                wait.await.unwrap();
                let _ = sender.send(Err(unknown()));
                Ok(())
            },
        )
        .unwrap();
    claimed.await.unwrap();
    drop(rx);
    let drain = tokio::spawn({
        let m = m.clone();
        async move { m.drain().await }
    });
    tokio::task::yield_now().await;
    assert!(!drain.is_finished());
    release.send(()).unwrap();
    assert_eq!(drain.await.unwrap(), Ok(()));
    assert!(
        m.admit(
            "two".into(),
            "d".into(),
            "a".into(),
            false,
            |_, _, _| async { Ok(()) }
        )
        .is_err()
    );
}

#[tokio::test]
async fn manager_capacity_pending_stop_compares_sealed_revision_and_keeps_fence() {
    let m = Manager::default();
    let (ready, waiting) = oneshot::channel();
    let (release, wait) = oneshot::channel();
    let rx = m
        .admit(
            "later".into(),
            "d".into(),
            "a".into(),
            false,
            |slot, cancel, sender| async move {
                slot.before_claim().unwrap();
                ready.send(()).unwrap();
                wait.await.unwrap();
                slot.issued(Some(3)).unwrap();
                assert!(!*cancel.borrow());
                let _ = sender.send(Err(unknown()));
                Ok(())
            },
        )
        .unwrap();
    waiting.await.unwrap();
    m.stop_assignment("a", 1).unwrap();
    release.send(()).unwrap();
    assert!(rx.await.unwrap().is_err());
    assert_eq!(m.settle_capacity_for_test().await, Ok(()));
    let rx = m
        .admit(
            "older".into(),
            "d".into(),
            "a".into(),
            false,
            |slot, _, sender| async move {
                slot.before_claim().unwrap();
                assert!(slot.issued(Some(1)).is_err());
                let _ = sender.send(Err(unknown()));
                Err(DrainFailure::Unknown)
            },
        )
        .unwrap();
    assert!(rx.await.unwrap().is_err());
    assert_eq!(m.drain().await, Err(DrainFailure::Unknown));
    assert_eq!(m.state.lock().unwrap().stops.get("a"), Some(&1));
}

#[tokio::test]
async fn manager_capacity_unknown_and_panicked_entries_remain_sticky() {
    let m = Manager::default();
    let rx = m
        .admit(
            "unknown".into(),
            "d".into(),
            "a".into(),
            false,
            |slot, _, sender| async move {
                slot.before_claim().unwrap();
                let _ = sender.send(Err(unknown()));
                Err(DrainFailure::Unknown)
            },
        )
        .unwrap();
    assert!(rx.await.unwrap().is_err());
    let rx = m
        .admit(
            "panic".into(),
            "d".into(),
            "a".into(),
            false,
            |_, _, _| async { panic!("synthetic failed cleanup") },
        )
        .unwrap();
    assert!(rx.await.is_err());
    assert_eq!(
        m.settle_capacity_for_test().await,
        Err(DrainFailure::Unknown)
    );
    let holds = m.hold_capacity_for_test(MAX_PROCESSES - 2);
    assert_eq!(m.state.lock().unwrap().entries.len(), MAX_PROCESSES);
    assert!(
        m.admit(
            "overflow".into(),
            "d".into(),
            "a".into(),
            false,
            |_, _, _| async { Ok(()) }
        )
        .is_err()
    );
    for tx in holds {
        tx.send(()).unwrap();
    }
    assert_eq!(m.drain().await, Err(DrainFailure::Unknown));
    assert!(m.state.lock().unwrap().entries.contains_key("unknown"));
    assert!(m.state.lock().unwrap().entries.contains_key("panic"));
}

/// Stop pauses an Agent. A paused Agent can be run again, so Stop keeps the
/// saved session and the work folder, both for a turn that was running and
/// for a conversation that was idle.
#[test]
fn owner_stop_keeps_shared_claude_sessions_and_work_folders() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap().join("native");
    let m = Manager::default();
    let (running, idle) = ("a".repeat(64), "b".repeat(64));
    let other_room = m
        .claude_workspaces
        .open(&base, &idle, "assignment", 1)
        .unwrap();
    let idle_path = other_room.path().to_path_buf();
    std::fs::write(idle_path.join("config/session.jsonl"), "idle conversation").unwrap();
    std::fs::write(idle_path.join("work/notes"), "idle work").unwrap();
    other_room.seal_stopped().unwrap();
    drop(other_room);
    let turn = m
        .claude_workspaces
        .open(&base, &running, "assignment", 1)
        .unwrap();
    let path = turn.path().to_path_buf();
    std::fs::write(path.join("config/session.jsonl"), "running conversation").unwrap();
    std::fs::write(path.join("work/notes"), "work in progress").unwrap();
    m.stop_assignment("assignment", 1).unwrap();
    // The stopped process tree is sealed the same way as after a finished turn.
    turn.seal_stopped().unwrap();
    drop(turn);
    m.stop_assignment("assignment", 2).unwrap();
    for (key, path, session, work) in [
        (&running, &path, "running conversation", "work in progress"),
        (&idle, &idle_path, "idle conversation", "idle work"),
    ] {
        assert!(base.join("sealed").join(key).join("manifest").exists());
        assert!(!path.join("config").exists());
        let resumed = m
            .claude_workspaces
            .open(&base, key, "assignment", 1)
            .unwrap();
        assert_eq!(resumed.path(), path.as_path());
        assert_eq!(
            std::fs::read_to_string(path.join("config/session.jsonl")).unwrap(),
            session
        );
        assert_eq!(
            std::fs::read_to_string(path.join("work/notes")).unwrap(),
            work
        );
    }
}

impl Manager {
    #[cfg(feature = "shared-intake")]
    pub(in crate::console::native) fn retains_command_for_test(&self, command: &str) -> bool {
        self.state.lock().unwrap().entries.contains_key(command)
    }
}
