use super::*;
use crate::logout::logout_confirm_modal::{LogoutConfirmModal, LogoutConfirmModalAction};
use hagency_store::credential_vault::{Error as VaultError, SecretRef, SecretStore, Vault};
use makepad_widgets::*;
use std::sync::{Mutex as StdMutex, atomic::AtomicUsize};
use zeroize::Zeroizing;

fn isolated(name: &str) -> bool {
    if std::env::var("HAGENCY_LOGOUT_REGRESSION_CHILD").as_deref() == Ok(name) {
        return false;
    }
    let profile = tempfile::tempdir().unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env("HAGENCY_LOGOUT_REGRESSION_CHILD", name)
        .env("HAGENCY_NATIVE_CREDENTIAL_IO", "deny")
        .env("HAGENCY_DESKTOP_DATA_DIR", profile.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let timed_out = loop {
        if child.try_wait().unwrap().is_some() {
            break false;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            break true;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let output = child.wait_with_output().unwrap();
    assert!(
        !timed_out,
        "original logout regression did not finish: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

#[derive(Debug, Clone)]
enum Observed {
    Failure(String),
    Progress(bool),
    Success {
        epoch: u64,
        revocation_pending: bool,
    },
}
fn action_context() -> (Cx, Arc<StdMutex<Vec<Observed>>>) {
    let captured = Arc::new(StdMutex::new(Vec::new()));
    let output = captured.clone();
    let cx = Cx::new(Box::new(move |_, event| {
        if let Event::Actions(actions) = event {
            for action in actions.iter() {
                let value = match action.downcast_ref::<LogoutAction>() {
                    Some(LogoutAction::LogoutFailure { message, .. }) => {
                        Some(Observed::Failure(message.clone()))
                    }
                    Some(LogoutAction::InProgress(value)) => Some(Observed::Progress(*value)),
                    Some(LogoutAction::LogoutSuccess {
                        epoch,
                        revocation_pending,
                    }) => Some(Observed::Success {
                        epoch: *epoch,
                        revocation_pending: *revocation_pending,
                    }),
                    _ => None,
                };
                if let Some(value) = value {
                    output.lock().unwrap().push(value);
                }
            }
        }
    }));
    (cx, captured)
}

#[derive(Default)]
struct LockedStore(AtomicUsize);
impl SecretStore for LockedStore {
    fn get(&self, _: &SecretRef) -> std::result::Result<Zeroizing<Vec<u8>>, VaultError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(VaultError::Locked)
    }
    fn put(&self, _: &SecretRef, _: &[u8]) -> std::result::Result<(), VaultError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(VaultError::Locked)
    }
    fn remove(&self, _: &SecretRef) -> std::result::Result<(), VaultError> {
        panic!("failed cleanup cannot delete native entries")
    }
}

#[test]
fn credential_migration_logout_cancellation_and_real_cleanup_failure_reach_ui() {
    let name = "logout::logout_state_machine::tests::credential_migration_logout_cancellation_and_real_cleanup_failure_reach_ui";
    if isolated(name) {
        return;
    }
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (mut cx, observed) = action_context();
    runtime.block_on(async {
        let cancelled = LogoutStateMachine::new(LogoutConfig::default());
        set_logout_point_of_no_return(false);
        set_logout_in_progress(true);
        cancelled.request_cancellation();
        assert!(
            cancelled
                .transition_to(LogoutState::PreChecking, "checking".into(), 10)
                .await
                .is_err()
        );
        assert_eq!(
            cancelled.current_state().await,
            LogoutState::Failed(LogoutError::Recoverable(RecoverableError::Cancelled))
        );
        assert!(!is_logout_in_progress());

        // Hold an actual transition boundary while the existing cancellation
        // request arrives. Once durable retirement is recorded, that request
        // must not interrupt later cleanup or resurrect authentication.
        let retired = Arc::new(LogoutStateMachine::new(LogoutConfig::default()));
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let original = retired.clone();
        let transition = tokio::spawn(async move {
            entered_tx.send(()).unwrap();
            release_rx.await.unwrap();
            original.point_of_no_return.store(true, Ordering::Release);
            set_logout_point_of_no_return(true);
            original
                .transition_to(LogoutState::ClosingTabs, "closing".into(), 60)
                .await
        });
        entered_rx.await.unwrap();
        retired.request_cancellation();
        release_tx.send(()).unwrap();
        transition.await.unwrap().unwrap();
        assert_eq!(retired.current_state().await, LogoutState::ClosingTabs);

        for filesystem_fault in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let store = Arc::new(LockedStore::default());
            let service = Arc::new(
                crate::persistence::credential_state::Service::new(
                    root.path().to_owned(),
                    Vault::new(store.clone()),
                )
                .unwrap(),
            );
            let session = crate::persistence::credential_state::tests::session_fixture();
            let owner = session.user_session.meta().user_id.clone();
            let account = crate::accounts::account_dir(root.path(), &owner);
            hagency_store::private::directory(&account).unwrap();
            let state = account.join("persistent_state");
            hagency_store::private::directory(&state).unwrap();
            let path = state.join("session");
            let bytes = serde_json::to_vec(&session).unwrap();
            hagency_store::private::write_new(&path, &bytes).unwrap();
            if filesystem_fault {
                std::fs::rename(&path, state.join("original-session")).unwrap();
                std::fs::create_dir(&path).unwrap();
            }
            set_logout_in_progress(true);
            let machine = LogoutStateMachine::new(LogoutConfig::default());
            machine.point_of_no_return.store(true, Ordering::Release);
            let result = machine
                .finish_account_cleanup(crate::persistence::credential_state::remove_account_on(
                    service.clone(),
                    path.clone(),
                    owner,
                ))
                .await;
            assert!(result.is_err());
            assert!(matches!(
                machine.current_state().await,
                LogoutState::Failed(LogoutError::Unrecoverable(_))
            ));
            assert!(
                is_logout_in_progress(),
                "retired cleanup failure must keep new account operations fenced until restart"
            );
            let original = if filesystem_fault {
                state.join("original-session")
            } else {
                path
            };
            assert_eq!(std::fs::read(original).unwrap(), bytes);
            if !filesystem_fault {
                assert!(store.0.load(Ordering::SeqCst) > 0);
            }
            assert!(crate::persistence::credential_state::test_shutdown(
                &service
            ));
        }
    });
    cx.handle_action_receiver();
    let events = observed.lock().unwrap();
    assert!(
        events
            .iter()
            .any(|value| matches!(value, Observed::Progress(false)))
    );
    let failures: Vec<_> = events
        .iter()
        .filter_map(|value| match value {
            Observed::Failure(message) => Some(message),
            _ => None,
        })
        .collect();
    assert_eq!(failures.len(), 3);
    let local_failure = crate::i18n::tr(
        "Local sign-out is durable. Local session cleanup failed; restart before signing in again. Any pending remote revocation will be retried.",
    );
    assert_eq!(
        failures
            .iter()
            .filter(|message| message.contains(local_failure))
            .count(),
        2
    );
    assert!(
        failures
            .iter()
            .all(|message| !message.contains("SECRET_MARKER"))
    );
    assert!(
        !events
            .iter()
            .any(|value| matches!(value, Observed::Success { .. }))
    );
}

#[test]
fn credential_migration_logout_actual_modal_keeps_pending_and_has_no_false_abort() {
    let name = "logout::logout_state_machine::tests::credential_migration_logout_actual_modal_keeps_pending_and_has_no_false_abort";
    if isolated(name) {
        return;
    }
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let mut modal = cx.with_vm(|vm| {
        makepad_widgets::script_mod(vm);
        crate::app::register_widgets(vm);
        vm.bx.captured_errors = Some(Vec::new());
        let value = script_eval!(vm, {mod.widgets.LogoutConfirmModal {}});
        let modal = LogoutConfirmModal::script_from_value(vm, value);
        assert!(vm.take_errors().is_empty());
        modal
    });
    let deliver = |cx: &mut Cx, modal: &mut LogoutConfirmModal, action: LogoutAction| {
        let actions = cx.capture_actions(|cx| cx.action(action));
        modal.handle_actions(cx, &actions, &mut Scope::empty());
    };
    deliver(
        &mut cx,
        &mut modal,
        LogoutAction::ProgressUpdate {
            epoch: crate::account_session::epoch(),
            message: "actual work remains active".into(),
            percentage: 45,
        },
    );
    let cancel = modal
        .test_view()
        .button(&cx, ids!(cancel_button))
        .widget_uid();
    let click = cx.capture_actions(|cx| {
        cx.widget_action(cancel, ButtonAction::Clicked(KeyModifiers::default()))
    });
    let result = cx.capture_actions(|cx| modal.handle_actions(cx, &click, &mut Scope::empty()));
    assert!(!result.iter().any(|action| matches!(
        action.downcast_ref::<LogoutConfirmModalAction>(),
        Some(LogoutConfirmModalAction::Close { .. })
    )));
    deliver(
        &mut cx,
        &mut modal,
        LogoutAction::LogoutSuccess {
            revocation_pending: true,
            epoch: crate::account_session::epoch(),
        },
    );
    assert_eq!(
        modal.test_view().label(&cx, ids!(body)).text(),
        crate::i18n::tr("Signed out locally. Remote token revocation remains pending.")
    );
    set_logout_point_of_no_return(true);
    deliver(
        &mut cx,
        &mut modal,
        LogoutAction::LogoutFailure {
            message: "Local session cleanup remains incomplete".into(),
            epoch: crate::account_session::epoch(),
        },
    );
    assert!(
        modal
            .test_view()
            .label(&cx, ids!(body))
            .text()
            .contains("Local session cleanup remains incomplete")
    );
    assert!(
        !modal
            .test_view()
            .label(&cx, ids!(body))
            .text()
            .contains("communicating with the homeserver")
    );
}

#[test]
fn credential_migration_logout_completion_keeps_original_epoch_after_releasing_account_gate() {
    let name = "logout::logout_state_machine::tests::credential_migration_logout_completion_keeps_original_epoch_after_releasing_account_gate";
    if isolated(name) {
        return;
    }
    let (mut cx, observed) = action_context();
    let mut modal = cx.with_vm(|vm| {
        makepad_widgets::script_mod(vm);
        crate::app::register_widgets(vm);
        vm.bx.captured_errors = Some(Vec::new());
        let value = script_eval!(vm, {mod.widgets.LogoutConfirmModal {}});
        let modal = LogoutConfirmModal::script_from_value(vm, value);
        assert!(vm.take_errors().is_empty());
        modal
    });
    let original_epoch = crate::account_session::epoch();
    set_logout_in_progress(true);
    let action = cx.capture_actions(|cx| {
        cx.action(LogoutAction::ProgressUpdate {
            message: "old operation".into(),
            percentage: 90,
            epoch: original_epoch,
        })
    });
    modal.handle_actions(&mut cx, &action, &mut Scope::empty());
    let (entered, at_released_gate) = std::sync::mpsc::sync_channel(1);
    let (release, wait_release) = std::sync::mpsc::sync_channel(1);
    let old = std::thread::spawn(move || {
        let machine = LogoutStateMachine::new(LogoutConfig::default());
        machine.revocation_pending.store(true, Ordering::Release);
        machine.publish_success_with(|| {
            entered.send(()).unwrap();
            wait_release.recv_timeout(Duration::from_secs(10)).unwrap();
        });
    });
    at_released_gate
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    assert!(!is_logout_in_progress());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(crate::account_session::close());
    crate::account_session::open();
    let next_epoch = crate::account_session::epoch();
    assert_ne!(next_epoch, original_epoch);
    release.send(()).unwrap();
    old.join().unwrap();
    cx.handle_action_receiver();
    let (epoch, revocation_pending) = observed
        .lock()
        .unwrap()
        .iter()
        .find_map(|event| match event {
            Observed::Success {
                epoch,
                revocation_pending,
            } => Some((*epoch, *revocation_pending)),
            _ => None,
        })
        .expect("the actual production completion must be posted");
    assert_eq!(
        epoch, original_epoch,
        "old completion cannot borrow the next account epoch"
    );
    assert_ne!(
        epoch, next_epoch,
        "App's actual epoch guard must reject this old result"
    );
    modal.test_reopen(&mut cx);
    let body = modal.test_view().label(&cx, ids!(body)).text();
    assert_eq!(body, crate::i18n::tr("Are you sure you want to logout?"));
    for late in [
        LogoutAction::LogoutSuccess {
            epoch,
            revocation_pending,
        },
        LogoutAction::LogoutFailure {
            message: "stale failure".into(),
            epoch,
        },
        LogoutAction::ProgressUpdate {
            message: "stale progress".into(),
            percentage: 100,
            epoch,
        },
        LogoutAction::ApplicationRequiresRestart {
            cleared_component: ClearedComponentType::Client,
            epoch,
        },
    ] {
        let action = cx.capture_actions(|cx| cx.action(late));
        modal.handle_actions(&mut cx, &action, &mut Scope::empty());
        assert_eq!(modal.test_view().label(&cx, ids!(body)).text(), body);
    }
}
