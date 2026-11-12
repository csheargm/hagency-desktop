//! Actual child process, stdio protocol, SDK loopback and SQLite. Offline provider fixture.
use super::*;
use hagency_store::management::{
    AccountLoginRequest, AccountInventoryQuery, PrepareAccount, Permission, Intent, IntentOperation,
};
use std::path::PathBuf;
fn package() -> ManagementPackage {
    ManagementPackage {
        digest: "c".repeat(64),
        instance: 999,
        account_epoch: 1,
        permissions: vec![
            Permission::ReadAccounts,
            Permission::PrepareAccounts,
            Permission::LoginAccounts,
            Permission::ReadAccountModels,
            Permission::ReadIntents,
            Permission::AcknowledgeIntents,
        ],
    }
}
fn executable(f: &Fixture) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = f._root.path().join("offline-account-login");
    std::fs::write(&path,r#"#!/usr/bin/env python3
import json,os,sys,pathlib
home=pathlib.Path(os.environ['CODEX_HOME']); assert home==pathlib.Path.cwd()
if sys.platform=='darwin':
 import pwd
 assert pathlib.Path(os.environ['HOME'])==pathlib.Path(pwd.getpwuid(os.geteuid()).pw_dir).resolve()
else:assert pathlib.Path(os.environ['HOME'])==home
assert not any(k in os.environ for k in ['OPENAI_API_KEY','CODEX_API_KEY','AZURE_OPENAI_API_KEY'])
(home/'fixture-pid').write_text(str(os.getpid()))
with (home/'fixture-spawns').open('a') as f:f.write('spawn\n')
for line in sys.stdin:
 r=json.loads(line);i=r.get('id');m=r.get('method')
 if i is None:continue
 if m=='initialize':v={'userAgent':'fixture/0.167.0'}
 elif m=='config/read':v={'config':{'model_provider':'openai','chatgpt_base_url':'https://chatgpt.com/backend-api'}}
 elif m=='account/read':v={'requiresOpenaiAuth':True,'account':({'type':'chatgpt','email':('changed@example.test' if (home/'fixture-switched').exists() else 'not-for-mini-app@example.test'),'planType':'plus'} if (home/'fixture-complete').exists() else None)}
 elif m=='model/list':
  (home/'fixture-model-entered').write_text('')
  if (home/'fixture-hold').exists():
   import time
   until=time.monotonic()+8
   while (home/'fixture-hold').exists() and time.monotonic()<until:time.sleep(.02)
  if (home/'fixture-switch').exists():(home/'fixture-switched').write_text('')
  v={'data':[{'hidden':False,'id':'coding','model':'gpt-5.6-sol','displayName':'Coding model','description':'Description','isDefault':True,'defaultReasoningEffort':'high','supportedReasoningEfforts':[{'reasoningEffort':'high','description':'High'}]}],'nextCursor':None}
 elif m=='account/login/start':
  with (home/'fixture-starts').open('a') as f:f.write('start\n')
  v={'type':'chatgpt','loginId':'original-login','authUrl':'https://auth.openai.com/authorize?state=offline-fixture'}
 elif m=='account/login/cancel':v={}
 else:raise Exception('No inference, logout, resource or credential operation permitted')
 print(json.dumps({'id':i,'result':v}),flush=True)
"#).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    *f.owner.0.account_logins.executable.lock().unwrap() = Some(path.clone());
    path
}
async fn input(
    f: &Fixture,
) -> (
    Arc<crate::native_owner::NativeManagement>,
    AccountLoginRequest,
) {
    executable(f);
    let m = f.owner.management(package()).await.unwrap();
    let account = m
        .prepare_account(PrepareAccount {
            command_id: "private-account".into(),
        })
        .await
        .unwrap();
    let inventory = m
        .account_inventory(AccountInventoryQuery { id: Some(account.account_id.clone()), after: None })
        .await
        .unwrap();
    let request = AccountLoginRequest {
        command_id: "login-original".into(),
        account_id: account.account_id,
        expected_revision: inventory.account.unwrap().revision.unwrap(),
    };
    (m, request)
}
async fn wait(
    m: &crate::native_owner::NativeManagement,
    input: &AccountLoginRequest,
    expected: &str,
) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let status = m.account_login_status(input.clone()).await.unwrap();
            if status.state == expected {
                break;
            }
            if !matches!(
                status.state.as_str(),
                "starting" | "awaiting_authorization" | "stopping"
            ) {
                panic!("expected {expected}, observed {}", status.state);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
}
fn find(parent: &std::path::Path, name: &str) -> Option<PathBuf> {
    for e in std::fs::read_dir(parent).ok()?.flatten() {
        let p = e.path();
        if p.file_name().is_some_and(|n| n == name) {
            return Some(p);
        }
        if p.is_dir()
            && let Some(found) = find(&p, name)
        {
            return Some(found);
        }
    }
    None
}
#[tokio::test]
async fn native_managed_login_selected_namespace_completion_and_original_replay() {
    let f = Fixture::new().await;
    let (m, input) = input(&f).await;
    m.login_account(input.clone()).await.unwrap();
    wait(&m, &input, "awaiting_authorization").await;
    let url = m
        .account_login_authorization(input.clone())
        .await
        .unwrap()
        .into_url();
    assert!(url.starts_with("https://auth.openai.com/"));
    let public =
        serde_json::to_string(&m.account_login_status(input.clone()).await.unwrap()).unwrap();
    assert!(
        !public.contains("authUrl")
            && !public.contains("state=offline")
            && !public.contains("@example")
    );
    m.login_account(input.clone()).await.unwrap();
    let namespace = find(f._root.path(), &input.account_id).unwrap();
    assert_eq!(
        std::fs::read_to_string(namespace.join("fixture-spawns"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    std::fs::write(namespace.join("fixture-complete"), "").unwrap();
    wait(&m, &input, "authenticated").await;
    assert_eq!(
        m.account_inventory(AccountInventoryQuery { id: Some(input.account_id.clone()), after: None })
            .await
            .unwrap()
            .account
            .unwrap()
            .readiness,
        "subscription"
    );
    assert!(m.account_login_authorization(input.clone()).await.is_err());
    let retry = m
        .retry_saved_command(input.command_id.clone())
        .await
        .unwrap();
    assert_eq!(retry["data"]["state"], "authenticated");
    assert_eq!(
        std::fs::read_to_string(namespace.join("fixture-starts"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    f.owner.suspend().await.unwrap();
    f.stop().await;
}
#[tokio::test]
async fn native_managed_login_cancel_stops_original_process_without_new_attempt() {
    let f = Fixture::new().await;
    let (m, input) = input(&f).await;
    m.login_account(input.clone()).await.unwrap();
    wait(&m, &input, "awaiting_authorization").await;
    m.cancel_account_login(input.clone()).await.unwrap();
    wait(&m, &input, "cancelled").await;
    assert!(m.account_login_authorization(input.clone()).await.is_err());
    assert_eq!(
        m.login_account(input.clone()).await.unwrap().state,
        "cancelled"
    );
    let namespace = find(f._root.path(), &input.account_id).unwrap();
    assert_eq!(
        std::fs::read_to_string(namespace.join("fixture-spawns"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert_eq!(
        m.account_inventory(AccountInventoryQuery { id: Some(input.account_id.clone()), after: None })
            .await
            .unwrap()
            .account
            .unwrap()
            .readiness,
        "unknown"
    );
    f.owner.suspend().await.unwrap();
    f.stop().await;
}
#[tokio::test]
async fn native_managed_login_sdk_loss_drains_and_cannot_deliver_authorization() {
    let f = Fixture::new().await;
    let (m, input) = input(&f).await;
    m.login_account(input.clone()).await.unwrap();
    wait(&m, &input, "awaiting_authorization").await;
    f.current.store(false, Ordering::Release);
    assert!(m.account_login_authorization(input.clone()).await.is_err());
    f.owner.0.account_logins.drain().await.unwrap();
    f.current.store(true, Ordering::Release);
    assert_ne!(
        m.account_login_status(input.clone()).await.unwrap().state,
        "authenticated"
    );
    assert_eq!(
        m.account_inventory(AccountInventoryQuery { id: Some(input.account_id.clone()), after: None })
            .await
            .unwrap()
            .account
            .unwrap()
            .readiness,
        "unknown"
    );
    f.owner.suspend().await.unwrap();
    f.stop().await;
}
#[tokio::test]
async fn native_managed_login_reserved_before_spawn_recovers_without_provider_after_restart() {
    let f = Fixture::new().await;
    let (m, input) = input(&f).await;
    let (context, session) = m.authorized().await.unwrap();
    session
        .prepare_intent(
            &context,
            Intent::new(
                IntentOperation::LoginAccount,
                input.command_id.clone(),
                &input,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let reservation = session
        .begin_account_login(&context, input.clone())
        .await
        .unwrap();
    drop(reservation);
    let Fixture {
        _root,
        owner,
        transport,
        registry,
        state: _,
        task,
        current,
    } = f;
    let path = _root.path().join("host");
    let origin = owner.0.expected_origin.clone();
    owner.suspend().await.unwrap();
    drop(session);
    drop(m);
    drop(transport);
    drop(owner);
    let selection=crate::native_owner::CurrentProfileSelection::load_for_isolated_tests(&path,&origin,"@owner:test",registry.clone()).unwrap().unwrap();
    let owner=selection.open_with_matrix_guarded(&path,&origin,"@owner:test",Arc::new(Source(current)),|_|Ok(())).await.unwrap();
    let m = owner.management(package()).await.unwrap();
    let result = m
        .retry_saved_command(input.command_id.clone())
        .await
        .unwrap();
    assert_eq!(result["data"]["state"], "uncertain");
    assert!(find(_root.path(), "fixture-spawns").is_none());
    let mut denied = package();
    denied.instance = 1000;
    denied
        .permissions
        .retain(|p| *p != Permission::LoginAccounts);
    let reader = owner.management(denied).await.unwrap();
    assert!(reader.account_login_status(input.clone()).await.is_err());
    assert!(reader.retry_saved_command(input.command_id).await.is_err());
    owner.suspend().await.unwrap();
    registry.shutdown().await.unwrap();
    task.abort();
}

fn model_query(input: &AccountLoginRequest) -> crate::native_owner::AccountModelQuery {
    crate::native_owner::AccountModelQuery {
        account_id: input.account_id.clone(),
        expected_revision: input.expected_revision.clone(),
    }
}
#[tokio::test]
async fn native_account_models_observe_only_selected_namespace_without_login_or_inference() {
    let f = Fixture::new().await;
    let (m, input) = input(&f).await;
    let namespace = find(f._root.path(), &input.account_id).unwrap();
    let error = m.account_models(model_query(&input)).await.unwrap_err();
    assert_eq!(error.code, "provider_connection_required");
    assert!(!namespace.join("fixture-starts").exists());
    std::fs::write(namespace.join("fixture-complete"), "").unwrap();
    let reply = m.account_models(model_query(&input)).await.unwrap();
    assert_eq!(reply["enrollmentReady"],true,"the current canonical account is ready for explicit enrollment");
    let mut enrollment_package=package();
    enrollment_package.instance+=20;
    // Model observation does not itself grant the separate enrollment action.
    let enroller=f.owner.management(enrollment_package).await.unwrap();
    let error=enroller.enroll_account_resource(super::super::NamespaceEnrollment{
        command_id:"ungranted-enrollment-not-started".into(),
        account:model_query(&input),model:"gpt-5.6-sol".into(),reasoning:Some("high".into()),monthly_tokens:1000,
    },std::time::Instant::now()+Duration::from_secs(30)).await.err().expect("enrollment needs its own current permission");
    assert_eq!(error.code,"management_access_required");
    assert!(!enroller.saved_commands().await.unwrap().iter().any(|r|r.command_id=="ungranted-enrollment-not-started"));

    assert_eq!(reply["accountId"], input.account_id);
    assert_eq!(reply["provider"]["models"][0]["model"], "gpt-5.6-sol");
    assert_eq!(
        reply["provider"]["models"][0]["sharedReasoning"],
        json!(["high"])
    );
    for forbidden in [
        "credentialRef",
        "authUrl",
        "not-for-mini-app",
        "CODEX_HOME",
        namespace.to_str().unwrap(),
    ] {
        assert!(!reply.to_string().contains(forbidden));
    }
    assert!(!namespace.join("fixture-starts").exists());
    assert_eq!(
        m.account_inventory(AccountInventoryQuery { id: Some(input.account_id.clone()), after: None })
            .await
            .unwrap()
            .account
            .unwrap()
            .readiness,
        "unknown"
    );
    f.owner.suspend().await.unwrap();
    f.stop().await;
}
#[tokio::test]
async fn native_account_models_refuse_changed_provider_and_lost_sdk() {
    let f = Fixture::new().await;
    let (m, input) = input(&f).await;
    let namespace = find(f._root.path(), &input.account_id).unwrap();
    std::fs::write(namespace.join("fixture-complete"), "").unwrap();
    std::fs::write(namespace.join("fixture-switch"), "").unwrap();
    assert_eq!(
        m.account_models(model_query(&input))
            .await
            .unwrap_err()
            .code,
        "provider_account_changed"
    );
    std::fs::remove_file(namespace.join("fixture-switch")).unwrap();
    std::fs::remove_file(namespace.join("fixture-switched")).unwrap();
    std::fs::remove_file(namespace.join("fixture-model-entered")).unwrap();
    std::fs::write(namespace.join("fixture-hold"), "").unwrap();
    let waiting = tokio::spawn({
        let m = m.clone();
        let query = model_query(&input);
        async move { m.account_models(query).await }
    });
    tokio::time::timeout(Duration::from_secs(10), async {
        while !namespace.join("fixture-model-entered").exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    f.current.store(false, Ordering::Release);
    std::fs::remove_file(namespace.join("fixture-hold")).unwrap();
    assert!(waiting.await.unwrap().is_err());
    f.current.store(true, Ordering::Release);
    f.owner.suspend().await.unwrap();
    f.stop().await;
}
#[tokio::test]
async fn native_account_models_do_not_probe_without_grant_or_during_login() {
    let f = Fixture::new().await;
    let (m, input) = input(&f).await;
    let namespace = find(f._root.path(), &input.account_id).unwrap();
    let mut limited = package();
    limited.instance = 1000;
    limited
        .permissions
        .retain(|p| *p != Permission::ReadAccountModels);
    let reader = f.owner.management(limited).await.unwrap();
    assert!(reader.account_models(model_query(&input)).await.is_err());
    assert!(!namespace.join("fixture-spawns").exists());
    m.login_account(input.clone()).await.unwrap();
    wait(&m, &input, "awaiting_authorization").await;
    assert!(m.account_models(model_query(&input)).await.is_err());
    assert_eq!(
        std::fs::read_to_string(namespace.join("fixture-spawns"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    m.cancel_account_login(input.clone()).await.unwrap();
    wait(&m, &input, "cancelled").await;
    f.owner.suspend().await.unwrap();
    f.stop().await;
}
