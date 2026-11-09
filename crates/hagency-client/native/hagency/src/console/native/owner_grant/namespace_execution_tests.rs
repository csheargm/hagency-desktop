#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_namespace_launch_after_current_setup_uses_selected_home_and_stops() {
    run_current_owner_contract("namespace").await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_idle_quota_after_current_setup_is_account_bound_and_revocable() {
    run_current_owner_contract("quota").await;
}
async fn verify_namespace_execution_contract(
    owner: &NativeOwner,
    registry: &OwnerResourceRegistry,
    profile: &str,
    root: &std::path::Path,
    quota_only: bool,
) {
    use hagency_store::management::{PrepareAccount, AccountInventoryQuery, AccountModelQuery};
    use hagency_store::owner_resources::NamespaceEnrollment;
    use super::super::managed_runtime::PreparedLaunch;
    use std::os::unix::fs::PermissionsExt;
    let mut package = current_owner_package(380);
    package.permissions.extend([
        Permission::PrepareAccounts,
        Permission::ReadAccounts,
        Permission::ReadAccountModels,
        Permission::ReadProviders,
        Permission::ConnectProviders,
        Permission::VerifyConnections,
        Permission::EnrollResources,
        Permission::ConfigureResources,
    ]);
    let manager = owner.management(package).await.unwrap();
    assert!(manager.account_enrollment_ready());
    let account = manager
        .prepare_account(PrepareAccount {
            command_id: "namespace-probe-account".into(),
        })
        .await
        .unwrap();
    let detail = manager
        .account_inventory(AccountInventoryQuery {
            id: Some(account.account_id.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
    let query = AccountModelQuery {
        account_id: account.account_id.clone(),
        expected_revision: detail.account.unwrap().revision.unwrap(),
    };
    let authority = ProfileAuthority::new(
        profile.into(),
        "@owner:test".into(),
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap();
    let selected = registry
        .select_resource_account(authority.clone(), query.clone())
        .await
        .unwrap();
    let mut env = std::collections::BTreeMap::new();
    selected.launch().apply_codex_environment(&mut env).unwrap();
    let namespace = PathBuf::from(env.get(std::ffi::OsStr::new("CODEX_HOME")).unwrap());
    assert!(namespace.ends_with(&query.account_id));
    let input = NamespaceEnrollment {
        command_id: "namespace-probe-resource".into(),
        account: query.clone(),
        model: "gpt-5.6-sol".into(),
        reasoning: Some("medium".into()),
        monthly_tokens: 1000,
    };
    let receipt = registry
        .prepare_namespace_enrollment(
            authority.clone(),
            input,
            selected,
            VerifiedProviderAccount::new(
                "codex",
                "openai.chatgpt",
                "independent-namespace-quota",
                Instant::now() + Duration::from_secs(30),
            )
            .unwrap(),
        )
        .unwrap()
        .approve()
        .await
        .unwrap();
    let resource = registry
        .resource(&authority, receipt.resource_id.clone())
        .await
        .unwrap();
    let python = std::process::Command::new("python3")
        .args(["-c", "import sys;print(sys.executable)"])
        .output()
        .unwrap();
    assert!(python.status.success());
    let python = String::from_utf8(python.stdout).unwrap();
    let python = python.trim();
    assert!(std::path::Path::new(python).is_absolute());
    assert!(!python.contains('\n'));
    Box::pin(verify_namespace_provider_contract(
        &manager, registry, &authority, &resource, &namespace, root, python,
    ))
    .await;
    if quota_only {
        *manager.owner.0.account_logins.executable.lock().unwrap() =
            Some(root.join("namespace-provider-probe.py"));
        Box::pin(verify_namespace_idle_quota_contract(
            &manager, &resource, &namespace,
        ))
        .await;
        *manager.owner.0.account_logins.executable.lock().unwrap() = None;
        manager.retire();
        return;
    }
    Box::pin(verify_namespace_enrollment_contract(
        &manager, registry, &authority, query, &namespace, root, profile,
    ))
    .await;
    let executable = root.join("namespace-runtime-probe.py");
    std::fs::write(&executable,format!("#!{python}\n" )+r#"import os,json,pathlib,sys
home=pathlib.Path(os.environ['CODEX_HOME'])
process_home=pathlib.Path(os.environ['HOME'])
if sys.platform=='darwin':
 import pwd
 assert process_home==pathlib.Path(pwd.getpwuid(os.geteuid()).pw_dir).resolve()
else:assert process_home==home
assert home.name.startswith('account_')
assert not any(k in os.environ for k in ['OPENAI_API_KEY','CODEX_API_KEY','AZURE_OPENAI_API_KEY'])
config={'model_providers':{},'mcp_servers':{},'features':{'future_default':True,'optional_configuration':{'enabled':True},'network_proxy':None}}
for index,arg in enumerate(sys.argv[1:]):
 if arg!='-c':continue
 key,value=sys.argv[index+2].split('=',1)
 try:value=json.loads(value)
 except ValueError:raise Exception('Fixture accepts only closed JSON configuration values')
 parts=key.split('.');target=config
 for part in parts[:-1]:target=target.setdefault(part,{})
 target[parts[-1]]=value
for line in sys.stdin:
 r=json.loads(line)
 if 'id' not in r:continue
 method=r['method']
 with (home/'namespace-runtime-methods').open('a') as f:f.write(method+'\n')
 if method=='initialize':
  with (home/'namespace-runtime-initialized').open('a') as f:f.write('initialized\n')
  value={'userAgent':'namespace-probe','platformFamily':'unix','platformOs':'fixture','codexHome':str(home)}
 elif method=='config/read':value={'config':config}
 elif method=='account/read':value={'requiresOpenaiAuth':True,'account':{'type':'chatgpt','email':None,'planType':'plus'}}
 elif method=='account/rateLimits/read':value={'accountId':'independent-namespace-quota','rateLimits':{}}
 else:raise Exception('No thread, turn, login or inference call permitted')
 print(json.dumps({'id':r['id'],'result':value}),flush=True)
"#).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let guardian = PathBuf::from(
        std::env::var_os("HAGENCY_TEST_RUNTIME_PROBE").expect("existing supervised runtime probe"),
    );
    let device = owner.0.host.console.authorized_device().await.unwrap();
    for text in [false, true] {
        let prepared = PreparedLaunch::for_resource_inner(
            root,
            &device,
            registry,
            &authority,
            &resource,
            text,
            Some((guardian.clone(), executable.clone())),
        )
        .await
        .unwrap();
        assert!(prepared.namespace().is_some());
        let runtime = super::super::managed_runtime::Manager::default();
        let deadline = Instant::now() + Duration::from_secs(10);
        let check = prepared.guard_metadata(Arc::new(|| Box::pin(async { Ok(()) })));
        let prepared = runtime.prepare(prepared, deadline, check).await.unwrap();
        let mut process = prepared.spawn(deadline).unwrap();
        process.initialize().await.unwrap();
        if text {
            process
                .qualify_shared_text(prepared.instructions())
                .await
                .unwrap();
        } else {
            process
                .qualify_managed_no_tools(prepared.instructions())
                .await
                .unwrap();
        }
        assert_eq!(
            process.stop_managed_observation().state(),
            hagency_runtime::owned::ManagedProcessState::Stopped
        );
        runtime.drain().await.unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(namespace.join("namespace-runtime-initialized"))
            .unwrap()
            .lines()
            .count(),
        4,
        "Each isolated namespace launch must stop its inspector before final qualification"
    );
    let methods = std::fs::read_to_string(namespace.join("namespace-runtime-methods")).unwrap();
    assert_eq!(
        methods
            .lines()
            .filter(|method| *method == "config/read")
            .count(),
        6
    );
    assert_eq!(
        methods
            .lines()
            .filter(|method| *method == "account/read")
            .count(),
        6
    );
    assert_eq!(
        methods
            .lines()
            .filter(|method| *method == "account/rateLimits/read")
            .count(),
        4
    );
    assert!(
        !methods.contains("thread/") && !methods.contains("turn/") && !methods.contains("login")
    );
    let selected = registry
        .resource_namespace(
            authority.clone(),
            resource.id.clone(),
            resource.revision.clone(),
        )
        .await
        .unwrap()
        .unwrap();
    selected.check_with(authority.clone()).await.unwrap();
    registry
        .prepare_revoke(
            authority,
            "namespace-probe-revoke".into(),
            resource.id,
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap()
        .approve()
        .await
        .unwrap();
    assert!(selected.check().await.is_err());
    manager.retire();
    drop(manager);
}

async fn verify_namespace_provider_contract(
    manager: &crate::native_owner::NativeManagement,
    registry: &OwnerResourceRegistry,
    authority: &ProfileAuthority,
    resource: &hagency_store::management::ResourceSummary,
    namespace: &std::path::Path,
    root: &std::path::Path,
    python: &str,
) {
    use std::os::unix::fs::PermissionsExt;
    let executable = root.join("namespace-provider-probe.py");
    std::fs::write(&executable,format!("#!{python}\n")+r#"import json,os,pathlib,sys,time
home=pathlib.Path(os.environ['CODEX_HOME'])
process_home=pathlib.Path(os.environ['HOME'])
if sys.platform=='darwin':
 import pwd
 assert process_home==pathlib.Path(pwd.getpwuid(os.geteuid()).pw_dir).resolve()
else:assert process_home==home
assert home.name.startswith('account_')
assert not any(k in os.environ for k in ['OPENAI_API_KEY','CODEX_API_KEY','AZURE_OPENAI_API_KEY'])
for line in sys.stdin:
 r=json.loads(line);i=r.get('id');m=r.get('method')
 if i is None:continue
 with (home/'namespace-provider-methods').open('a') as f:f.write(m+'\n')
 if m=='initialize':v={}
 elif m=='config/read':v={'config':{'model_provider':'openai','chatgpt_base_url':'https://chatgpt.com/backend-api'}}
 elif m=='account/read':
  if (home/'provider-quota-hold').exists():
   (home/'provider-quota-waiting').touch()
   limit=time.monotonic()+5
   while (home/'provider-quota-hold').exists() and time.monotonic()<limit:time.sleep(.01)
  v={'requiresOpenaiAuth':True,'account':None if (home/'provider-signed-out').exists() else {'type':'chatgpt','email':'private@example.test','planType':'plus'}}
 elif m=='account/rateLimits/read':v={'accountId':('other-quota' if (home/'provider-other-quota').exists() else 'independent-namespace-quota'),'rateLimits':{'primary':{'usedPercent':25,'windowDurationMins':300,'resetsAt':1900000000}}}
 elif m=='model/list':v={'data':[{'hidden':False,'id':'coding','model':'gpt-5.6-sol','displayName':'Coding','description':'Public','isDefault':True,'defaultReasoningEffort':'medium','supportedReasoningEfforts':[{'reasoningEffort':'medium','description':'Medium'},{'reasoningEffort':'high','description':'High'}]}],'nextCursor':None}
 else:raise Exception('No login, credential, thread or inference operation permitted')
 print(json.dumps({'id':i,'result':v}),flush=True)
"#).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    *manager.owner.0.account_logins.executable.lock().unwrap() = Some(executable);
    let proof = manager
        .verified_resource_provider_account(
            registry,
            authority.clone(),
            super::super::ManagementRuntime::Codex,
            resource.id.clone(),
            resource.revision.clone(),
        )
        .await
        .unwrap();
    registry
        .verify_provider_resource(authority.clone(), resource.id.clone(), proof)
        .await
        .unwrap();
    let (context, session) = manager.authorized().await.unwrap();
    let verified = manager
        .verify_local_resource(&context, &session, resource, false)
        .await
        .unwrap();
    assert_eq!(verified.revision, resource.revision);
    std::fs::write(namespace.join("provider-other-quota"), "").unwrap();
    assert!(
        manager
            .verify_local_resource(&context, &session, resource, false)
            .await
            .is_err()
    );
    std::fs::remove_file(namespace.join("provider-other-quota")).unwrap();
    std::fs::write(namespace.join("provider-signed-out"), "").unwrap();
    let error = manager
        .verify_local_resource(&context, &session, resource, false)
        .await
        .unwrap_err();
    assert_eq!(error.code, "provider_connection_required");
    std::fs::remove_file(namespace.join("provider-signed-out")).unwrap();
    assert_eq!(
        std::fs::read_to_string(namespace.join("namespace-provider-methods"))
            .unwrap()
            .lines()
            .filter(|m| *m == "initialize")
            .count(),
        4
    );
    *manager.owner.0.account_logins.executable.lock().unwrap() = None;
}

async fn verify_namespace_idle_quota_contract(
    manager: &crate::native_owner::NativeManagement,
    resource: &hagency_store::management::ResourceSummary,
    namespace: &std::path::Path,
) {
    manager.refresh_resource_quota(resource).await.unwrap();
    let reports = manager
        .owner
        .telemetry()
        .parts(now_ms().unwrap(), Default::default())
        .unwrap();
    assert_eq!(reports.accounts.len(), 1);
    assert_eq!(reports.accounts[0].plan.as_deref(), Some("plus"));
    assert!(
        reports
            .resources
            .iter()
            .any(|r| r.resource_id == resource.id && r.account_id.is_some() && r.executions == 0)
    );
    std::fs::write(namespace.join("provider-other-quota"), "").unwrap();
    assert!(
        manager.refresh_resource_quota(resource).await.is_err(),
        "an explicit namespace cannot report a replacement account against the original resource"
    );
    let reports = manager
        .owner
        .telemetry()
        .parts(now_ms().unwrap(), Default::default())
        .unwrap();
    assert!(
        !reports
            .resources
            .iter()
            .any(|r| r.resource_id == resource.id && r.account_id.is_some())
    );
    std::fs::remove_file(namespace.join("provider-other-quota")).unwrap();
    std::fs::write(namespace.join("provider-quota-hold"), b"").unwrap();
    let pending = manager.refresh_resource_quota(resource);
    tokio::pin!(pending);
    let wait = tokio::time::sleep(Duration::from_secs(3));
    tokio::pin!(wait);
    loop {
        tokio::select! {
            result = &mut pending => panic!("quota metadata must remain pending: {result:?}"),
            _ = &mut wait => panic!("quota fixture never reached its controlled account read"),
            _ = tokio::time::sleep(Duration::from_millis(10)) => {
                if namespace.join("provider-quota-waiting").exists() { break; }
            }
        }
    }
    manager.retire();
    std::fs::remove_file(namespace.join("provider-quota-hold")).unwrap();
    assert!(
        pending.await.is_err(),
        "retired authority cannot publish a late quota report"
    );
    let reports = manager
        .owner
        .telemetry()
        .parts(now_ms().unwrap(), Default::default())
        .unwrap();
    assert!(
        !reports
            .resources
            .iter()
            .any(|r| r.resource_id == resource.id && r.account_id.is_some())
    );
    assert!(manager.authorized().await.is_err());
}

async fn verify_namespace_enrollment_contract(
    manager: &crate::native_owner::NativeManagement,
    registry: &OwnerResourceRegistry,
    authority: &ProfileAuthority,
    account: hagency_store::management::AccountModelQuery,
    namespace: &std::path::Path,
    root: &std::path::Path,
    profile: &str,
) {
    use hagency_store::owner_resources::NamespaceEnrollment;
    let input = NamespaceEnrollment {
        command_id: "native-namespace-enrollment".into(),
        account,
        model: "gpt-5.6-sol".into(),
        reasoning: Some("high".into()),
        monthly_tokens: 1000,
    };
    *manager.owner.0.account_logins.executable.lock().unwrap() =
        Some(root.join("namespace-provider-probe.py"));
    let result = manager
        .enroll_account_resource(input.clone(), Instant::now() + Duration::from_secs(30))
        .await
        .unwrap();
    let resource = registry
        .resource(authority, result.resource_id.clone())
        .await
        .unwrap();
    assert!(!resource.published);
    assert_eq!(resource.reasoning.as_deref(), Some("high"));
    registry
        .resource_namespace(authority.clone(), resource.id, resource.revision)
        .await
        .unwrap()
        .unwrap()
        .check()
        .await
        .unwrap();
    let before = std::fs::read_to_string(namespace.join("namespace-provider-methods")).unwrap();
    *manager.owner.0.account_logins.executable.lock().unwrap() = Some(root.join("does-not-exist"));
    let repeated = manager
        .enroll_account_resource(input.clone(), Instant::now() + Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&result).unwrap(),
        serde_json::to_value(repeated).unwrap()
    );
    assert_eq!(
        std::fs::read_to_string(namespace.join("namespace-provider-methods")).unwrap(),
        before
    );
    let recovered = Box::pin(manager.retry_saved_command(input.command_id.clone()))
        .await
        .unwrap();
    assert_eq!(recovered["operation"], "enroll_account_resource");
    assert_eq!(recovered["data"], serde_json::to_value(&result).unwrap());
    assert_eq!(
        std::fs::read_to_string(namespace.join("namespace-provider-methods")).unwrap(),
        before,
        "saved native recovery uses the original result before provider RPC"
    );

    let mut changed = input.clone();
    changed.monthly_tokens = 2000;
    assert!(
        manager
            .enroll_account_resource(changed, Instant::now() + Duration::from_secs(30))
            .await
            .is_err()
    );
    assert!(
        manager
            .enroll_account_resource(input, Instant::now())
            .await
            .is_err()
    );
    Box::pin(verify_namespace_edit_contract(
        manager,
        registry,
        authority,
        &result.resource_id,
        namespace,
        root,
        profile,
    ))
    .await;
    *manager.owner.0.account_logins.executable.lock().unwrap() = None;
}

async fn verify_namespace_edit_contract(
    manager: &crate::native_owner::NativeManagement,
    registry: &OwnerResourceRegistry,
    authority: &ProfileAuthority,
    resource_id: &str,
    namespace: &std::path::Path,
    root: &std::path::Path,
    profile: &str,
) {
    use hagency_store::{management::EditLocalResource, ProfileChange, CeilingChange};
    let original = registry
        .resource(authority, resource_id.into())
        .await
        .unwrap();
    let make = |command: &str, model: &str| EditLocalResource {
        command_id: command.into(),
        resource_id: resource_id.into(),
        expected_revision: original.revision.clone(),
        profile: ProfileChange::Select {
            model: model.into(),
            reasoning: Some("medium".into()),
        },
        ceiling: CeilingChange::Monthly {
            tokens: 450.try_into().unwrap(),
        },
        execution_policy: None,
        expected_ceiling_sequence: None,
    };
    let before_review = manager.saved_commands().await.unwrap().len();
    let methods_before_review =
        std::fs::read_to_string(namespace.join("namespace-provider-methods")).unwrap();
    let review = manager
        .review_local_resource_edit(&make("native-review-only", "gpt-5.6-sol"))
        .await
        .unwrap();
    let crate::native_owner::LocalResourceEditReview::Editable(detail) = review else {
        panic!("new edit must show canonical values");
    };
    assert_eq!(detail.resource.revision, original.revision);
    assert_eq!(detail.resource.model, original.model);
    assert_eq!(
        manager.saved_commands().await.unwrap().len(),
        before_review,
        "cancelled review must not create an intent"
    );
    assert_eq!(
        std::fs::read_to_string(namespace.join("namespace-provider-methods")).unwrap(),
        methods_before_review,
        "review does not contact provider"
    );
    *manager.owner.0.account_logins.executable.lock().unwrap() =
        Some(root.join("namespace-provider-probe.py"));
    let invalid = manager
        .edit_local_resource(
            make("native-edit-invalid-model", "coding"),
            Instant::now() + Duration::from_secs(30),
        )
        .await
        .unwrap_err();
    assert_eq!(invalid.code, "provider_model_changed");
    assert_eq!(
        registry
            .resource(authority, resource_id.into())
            .await
            .unwrap()
            .revision,
        original.revision
    );
    let expired = manager
        .edit_local_resource(make("native-edit-expired", "gpt-5.6-sol"), Instant::now())
        .await
        .unwrap_err();
    assert_eq!(expired.code, "provider_confirmation_expired");
    // The canonical edit commits, then the original private reply fails. This
    // fixture changes only its own retained journal, never the running profile.
    let journal = root
        .join("owned-agent-owners")
        .join(format!("management_{profile}"))
        .join("server-management.sqlite3");
    let db = rusqlite::Connection::open(journal).unwrap();
    db.set_db_config(
        rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
        true,
    )
    .unwrap();
    db.execute_batch("CREATE TRIGGER reject_native_edit_reply BEFORE UPDATE ON management_intents WHEN NEW.command_id='native-edit-lost-reply' AND NEW.state='replied' BEGIN SELECT RAISE(ABORT,'fixture lost reply'); END;").unwrap();
    let error = manager
        .edit_local_resource(
            make("native-edit-lost-reply", "gpt-5.6-sol"),
            Instant::now() + Duration::from_secs(30),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "management_outcome_unknown");
    let edited = registry
        .resource(authority, resource_id.into())
        .await
        .unwrap();
    assert_ne!(edited.revision, original.revision);
    assert_eq!(edited.reasoning.as_deref(), Some("medium"));
    assert_eq!(edited.budget.pool.ceiling.map(u64::from), Some(450));
    assert_eq!(edited.budget.seat.quota, None);
    db.execute_batch("DROP TRIGGER reject_native_edit_reply")
        .unwrap();
    drop(db);
    let methods = std::fs::read_to_string(namespace.join("namespace-provider-methods")).unwrap();
    *manager.owner.0.account_logins.executable.lock().unwrap() = Some(root.join("does-not-exist"));
    let review = manager
        .review_local_resource_edit(&make("native-edit-lost-reply", "gpt-5.6-sol"))
        .await
        .unwrap();
    let crate::native_owner::LocalResourceEditReview::Recorded(saved) = review else {
        panic!("committed edit must be reviewed as recovery");
    };
    assert_eq!(saved.revision, edited.revision);
    assert_eq!(
        std::fs::read_to_string(namespace.join("namespace-provider-methods")).unwrap(),
        methods
    );
    assert_eq!(
        manager
            .review_local_resource_edit(&make("native-stale-review", "gpt-5.6-sol"))
            .await
            .unwrap_err()
            .code,
        "management_resource_edit_changed"
    );
    let recovered = Box::pin(manager.retry_saved_command("native-edit-lost-reply".into()))
        .await
        .unwrap();
    assert_eq!(recovered["operation"], "edit_local_resource");
    assert_eq!(recovered["data"]["revision"], edited.revision);
    assert_eq!(
        std::fs::read_to_string(namespace.join("namespace-provider-methods")).unwrap(),
        methods,
        "canonical receipt recovery precedes provider RPC after lost private reply"
    );
    let repeated = manager
        .edit_local_resource(
            make("native-edit-lost-reply", "gpt-5.6-sol"),
            Instant::now() + Duration::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(repeated.revision, edited.revision);
    assert_eq!(
        registry
            .resource(authority, resource_id.into())
            .await
            .unwrap()
            .revision,
        edited.revision
    );
    // A pending attempt followed by a temporary provider failure remains
    // recoverable. Never turn an uncertain original attempt into a new command.
    let deferred = EditLocalResource {
        command_id: "native-edit-deferred".into(),
        resource_id: resource_id.into(),
        expected_revision: edited.revision.clone(),
        profile: ProfileChange::Preserve {},
        ceiling: CeilingChange::Monthly {
            tokens: 400.try_into().unwrap(),
        },
        execution_policy: None,
        expected_ceiling_sequence: None,
    };
    assert!(
        manager
            .edit_local_resource(deferred, Instant::now() + Duration::from_secs(30))
            .await
            .is_err()
    );
    assert!(
        manager
            .saved_commands()
            .await
            .unwrap()
            .iter()
            .find(|c| c.command_id == "native-edit-deferred")
            .unwrap()
            .reply
            .is_none()
    );
    *manager.owner.0.account_logins.executable.lock().unwrap() =
        Some(root.join("namespace-provider-probe.py"));
    std::fs::write(namespace.join("provider-signed-out"), "").unwrap();
    let pending = Box::pin(manager.retry_saved_command("native-edit-deferred".into()))
        .await
        .unwrap_err();
    assert_eq!(pending.code, "management_outcome_unknown");
    assert!(
        manager
            .saved_commands()
            .await
            .unwrap()
            .iter()
            .find(|c| c.command_id == "native-edit-deferred")
            .unwrap()
            .reply
            .is_none()
    );
    assert_eq!(
        registry
            .resource(authority, resource_id.into())
            .await
            .unwrap()
            .revision,
        edited.revision
    );
    std::fs::remove_file(namespace.join("provider-signed-out")).unwrap();
    let recovered = Box::pin(manager.retry_saved_command("native-edit-deferred".into()))
        .await
        .unwrap();
    let current = registry
        .resource(authority, resource_id.into())
        .await
        .unwrap();
    assert_eq!(recovered["data"]["revision"], current.revision);
    assert_eq!(current.budget.pool.ceiling.map(u64::from), Some(400));
    assert_eq!(current.budget.seat.quota, None);
}
