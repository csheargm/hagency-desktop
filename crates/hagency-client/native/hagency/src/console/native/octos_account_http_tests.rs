//! Isolated owner/SDK fixture; the subprocess owns OCTOS_HOME and never reads
//! the person's Octos configuration or starts an agent.
use super::*;
use hagency_store::management::{
    AccountInventoryQuery, AccountModelQuery, AccountSummary, Intent, IntentOperation,
};
use hagency_store::owner_resources::NamespaceEnrollment;

#[test]
fn octos_accounts_are_scoped_paginated_and_recover_original_enrollment() {
    const CHILD: &str = "HAGENCY_OCTOS_ACCOUNT_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("octos");
        std::fs::create_dir_all(home.join("profiles")).unwrap();
        let module = module_path!()
            .strip_prefix("hagency::")
            .unwrap_or(module_path!());
        let test = format!(
            "{module}::octos_accounts_are_scoped_paginated_and_recover_original_enrollment"
        );
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg(test)
            .arg("--nocapture")
            .arg("--test-threads=1")
            .env(CHILD, "1")
            .env("OCTOS_HOME", &home)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&result.stdout);
        assert!(
            result.status.success() && stdout.contains("OCTOS_ACCOUNT_FIXTURE_COMPLETE"),
            "{}\n{}",
            stdout,
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }
    let home = std::path::PathBuf::from(std::env::var_os("OCTOS_HOME").unwrap())
        .canonicalize()
        .unwrap();
    let write = |id: &str, secret: &str| {
        std::fs::write(home.join("profiles").join(format!("{id}.json")),
        json!({"id":id,"config":{"llm":{"primary":{"family_id":"local","model_id":"same-model"},
            "fallbacks":if id == "profile_00" {json!([{"family_id":"local","model_id":"deepseek"}])} else {json!([])}},
            "env_vars":{"TOKEN":secret}}}).to_string()).unwrap()
    };
    for i in 0..20 {
        write(&format!("profile_{i:02}"), "fixture-secret");
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let f = Fixture::empty_current().await;
        let console = &f.owner.0.host.console;
        let device = console.authorized_device().await.unwrap();
        let root = console.0.server_login.state_directory().unwrap();
        crate::console::owner_provider::external::record_connection(
            &root,
            device.origin(),
            device.issuer(),
            device.subject(),
            device.owner_mxid(),
            hagency_agent_local::runtime::Kind::Octos,
        )
        .unwrap();
        let mut package = package(92);
        package.permissions.extend([
            Permission::ReadAccounts,
            Permission::ReadAccountModels,
            Permission::ReadResources,
            Permission::EnrollResources,
        ]);
        let m = f.owner.management(package).await.unwrap();
        let mut after = None;
        let mut accounts = Vec::new();
        loop {
            let page = m
                .account_inventory(AccountInventoryQuery { id: None, after })
                .await
                .unwrap();
            assert!(page.accounts.len() <= 16);
            if let Some(next) = &page.next_after {
                assert_eq!(page.accounts.last().map(|v| &v.id), Some(next));
            }
            accounts.extend(page.accounts);
            match page.next_after {
                Some(next) => after = Some(next),
                None => break,
            }
        }
        let octos = accounts
            .iter()
            .filter(|a| a.runtime == "octos")
            .collect::<Vec<_>>();
        assert_eq!(octos.len(), 20);
        assert!(
            octos
                .iter()
                .all(|a| a.resource_count == 0 && a.identity_key.is_none())
        );
        let public = serde_json::to_string(&accounts).unwrap();
        assert!(!public.contains("fixture-secret") && !public.contains(home.to_str().unwrap()));
        let first = octos
            .iter()
            .find(|a| a.profile.as_deref() == Some("profile_00"))
            .unwrap();
        let second = octos
            .iter()
            .find(|a| a.profile.as_deref() == Some("profile_01"))
            .unwrap();
        let query = |a: &AccountSummary| AccountModelQuery {
            account_id: a.id.clone(),
            expected_revision: a.revision.clone().unwrap(),
        };
        let choices = m.account_models(query(first)).await.unwrap();
        assert_eq!(choices["provider"]["runtime"], "octos");
        assert_eq!(choices["provider"]["accountVerified"], false);
        assert_eq!(choices["enrollmentReady"], cfg!(target_os = "macos"));
        assert_eq!(choices["provider"]["models"].as_array().unwrap().len(), 2);
        assert_eq!(choices["provider"]["models"][1]["model"], "deepseek");
        let request = |a: &AccountSummary, id: &str, tokens| NamespaceEnrollment {
            command_id: id.into(),
            account: query(a),
            model: "same-model".into(),
            reasoning: None,
            monthly_tokens: tokens,
        };
        let original = request(first, "octos_first", 1000);
        let second_request = request(second, "octos_second", 2000);
        let confirm = || std::time::Instant::now() + std::time::Duration::from_secs(30);
        if !cfg!(target_os = "macos") {
            match m.enroll_account_resource(original, confirm()).await {
                Err(error) => assert_eq!(error.code, "octos_pool_platform_unsupported"),
                Ok(_) => panic!("Octos enrollment must fail closed on unsupported platforms"),
            }
            f.stop().await;
            println!("OCTOS_ACCOUNT_FIXTURE_COMPLETE");
            return;
        }
        let one = m
            .enroll_account_resource(original.clone(), confirm())
            .await
            .unwrap();
        let two = m
            .enroll_account_resource(second_request, confirm())
            .await
            .unwrap();
        assert_ne!(one.resource_id, two.resource_id);
        let detail = m
            .account_inventory(AccountInventoryQuery {
                id: Some(first.id.clone()),
                after: None,
            })
            .await
            .unwrap();
        assert_eq!(detail.account.unwrap().resource_count, 1);
        assert_eq!(detail.resources.len(), 1);
        assert_eq!(detail.resources[0].id, one.resource_id);
        assert!(!detail.resources[0].published);
        let choices = m.account_models(query(first)).await.unwrap();
        assert_eq!(choices["provider"]["sharedQuota"], 1000);
        assert_eq!(choices["provider"]["models"][0]["enrolled"], true);
        assert_eq!(choices["provider"]["models"][1]["enrolled"], false);
        let alternative = NamespaceEnrollment {
            command_id: "octos_fallback".into(), account: query(first), model: "deepseek".into(),
            reasoning: None, monthly_tokens: 900,
        };
        let three = m.enroll_account_resource(alternative, confirm()).await.unwrap();
        assert_ne!(one.resource_id, three.resource_id);
        let choices = m.account_models(query(first)).await.unwrap();
        assert_eq!(choices["provider"]["sharedQuota"], 1000);
        assert_eq!(choices["provider"]["models"][1]["enrolled"], true);
        let detail = m.account_inventory(AccountInventoryQuery {id: Some(first.id.clone()), after: None}).await.unwrap();
        assert_eq!(detail.account.unwrap().resource_count, 2);
        assert_eq!(detail.resources.len(), 2);
        // Prepare the exact management intent and commit only the local
        // receipt; after a lost reply recovery must not read changed secrets.
        let third = octos
            .iter()
            .find(|a| a.profile.as_deref() == Some("profile_02"))
            .unwrap();
        let saved = request(third, "octos_lost_reply", 3000);
        let (context, session) = m.authorized().await.unwrap();
        session
            .prepare_intent(
                &context,
                Intent::new(
                    IntentOperation::EnrollAccountResource,
                    saved.command_id.clone(),
                    &saved,
                )
                .unwrap(),
            )
            .await
            .unwrap();
        let (registry, profile) = session.namespace_enrollment_registry(&context).unwrap();
        let witness = hagency_runtime::octos::profile_binding::ProfileBinding::observe(
            &home,
            "profile_02",
            confirm(),
        )
        .unwrap();
        let proof = registry.observe_octos_profile(witness).await.unwrap();
        let committed = registry
            .prepare_octos_account_enrollment(profile, saved.clone(), proof, confirm())
            .unwrap()
            .approve()
            .await
            .unwrap();
        write("profile_02", "rotated-after-commit");
        let recovered = m.retry_saved_command(saved.command_id).await.unwrap();
        assert_eq!(recovered["data"]["resourceId"], committed.resource_id);
        drop(session);
        drop(m);
        f.owner.suspend().await.unwrap();
        f.stop().await;
        println!("OCTOS_ACCOUNT_FIXTURE_COMPLETE");
    });
}
