/// The Nov 18 demo stall: while an Agent's turn renewed its authorization
/// against a slow server and another owner command waited on that server,
/// the owner's read of the Agent's waiting requests queued behind both and
/// was reported `approvals_read_timeout`; the cards expired unseen.
mod approvals_contention_tests {
    use super::super::super::super::native::LocalAction;
    use super::*;

    /// The desktop's per-queue bound (`approvals_read::QUEUE_WAIT`).
    const QUEUE_WAIT: Duration = Duration::from_secs(2);

    async fn owner() -> (NativeOwner, tokio::task::JoinHandle<()>, tempfile::TempDir) {
        let (origin, _requests, task) = fixture("@owner:test", None).await;
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let source = Arc::new(Source {
            token: std::sync::Mutex::new("sdk-first".into()),
            current: Arc::new(AtomicBool::new(true)),
            error_code: "matrix_account_changed",
            calls: std::sync::atomic::AtomicUsize::new(0),
            refreshes: std::sync::atomic::AtomicUsize::new(0),
        });
        let native = NativeOwner::open_with_matrix(&root.path().join("state"), &origin, "@owner:test", source)
            .await
            .unwrap();
        (native, task, root)
    }

    fn approvals_command() -> Command {
        Command::Local { agent_id: "agt_1".into(), action: LocalAction::Approvals, input: None, binding_id: None, requester: None }
    }

    #[tokio::test]
    async fn waiting_requests_are_readable_while_a_turn_holds_the_renewal_and_command_lines() {
        let (native, task, _root) = owner().await;
        let login = &native.test_console().0.server_login;
        // The turn's execution call is renewing against a slow server, and a
        // listing waits on the same server under the owner command line.
        let renewal = login.renewal.lock().await;
        let commands = native.hold_owner_commands().await;
        let before = tokio::time::timeout(QUEUE_WAIT, native.execute(approvals_command())).await;
        assert!(before.is_err(), "the command path queues behind both lines: the reported timeout");
        let read = tokio::time::timeout(QUEUE_WAIT, native.local_approvals("agt_1")).await;
        let value = read.expect("the in-memory queue answers within the bound").unwrap();
        assert_eq!(value, json!({"approvals": []}));
        drop((commands, renewal));
        native.shutdown().await;
        task.abort();
    }

    #[tokio::test]
    async fn a_lapsed_or_withdrawn_proof_is_never_read_as_live() {
        let (native, task, _root) = owner().await;
        let login = &native.test_console().0.server_login;
        for session in login.sessions.lock().await.values_mut() {
            session.authorized_until = Instant::now() + Duration::from_secs(1);
        }
        let renewal = login.renewal.lock().await;
        let read = tokio::time::timeout(Duration::from_millis(300), native.local_approvals("agt_1")).await;
        assert!(read.is_err(), "a proof about to lapse waits for its renewal");
        drop(renewal);
        for session in login.sessions.lock().await.values_mut() {
            session.authorized_until = Instant::now() + Duration::from_secs(30);
            session.invalidated = true;
        }
        let read = tokio::time::timeout(QUEUE_WAIT, native.local_approvals("agt_1")).await.unwrap();
        assert_eq!(read.unwrap_err().code, "owner_authorization_required");
        assert_eq!(native.local_approvals("../x").await.unwrap_err().code, "invalid_arguments");
        native.shutdown().await;
        task.abort();
    }
}
