use super::*;
fn account(email: &str) -> Value {
    json!({"requiresOpenaiAuth":true,"account":{"type":"chatgpt","email":email,"planType":"plus"}})
}
fn usage(id: &str) -> Value {
    json!({"accountId":id,"rateLimits":{"primary":{"usedPercent":20}}})
}
#[test]
fn identity_requires_public_usage_account_not_email_org_model_or_path() {
    for value in [
        json!({"accountId":"acct_123"}),
        json!({"rateLimits":{},"email":"same@example.test"}),
        json!({"rateLimits":{},"orgId":"org_123"}),
        json!({"rateLimits":{},"credentialRef":"codex-managed:shared-home:123"}),
        json!({"rateLimits":{},"accountId":null}),
        json!({"rateLimits":{},"accountId":7}),
        usage(""),
        usage("same@example.test"),
        usage("../account"),
        usage(" account"),
        usage("account\n"),
        usage(&"a".repeat(129)),
        usage("同一个账户"),
    ] {
        assert!(QuotaIdentity::read(&value).is_err());
    }
    let identity = QuotaIdentity::read(&usage("acct_123")).unwrap();
    assert_eq!(identity.0, "acct_123");
    assert!(
        identity
            .proof(Instant::now() + Duration::from_secs(30))
            .is_ok()
    );
}
#[test]
fn malformed_or_ambiguous_authentication_never_issues_identity() {
    for value in [
        json!({"requiresOpenaiAuth":false,"account":{"type":"chatgpt","email":"a@b","planType":"plus"}}),
        json!({"requiresOpenaiAuth":"true","account":{"type":"chatgpt","email":"a@b","planType":"plus"}}),
        json!({"requiresOpenaiAuth":true,"account":null}),
        json!({"requiresOpenaiAuth":true,"account":{"type":"apiKey"}}),
        json!({"loggedIn":true,"email":"a@b","orgId":"org_123"}),
        json!({"requiresOpenaiAuth":true,"account":{"type":"chatgpt","planType":"plus"}}),
        json!({"requiresOpenaiAuth":true,"account":{"type":"chatgpt","email":123,"planType":"plus"}}),
        json!({"requiresOpenaiAuth":true,"account":{"type":"chatgpt","email":null,"planType":null}}),
        account("a\nb"),
    ] {
        assert!(snapshot(&value).is_err());
    }
    assert!(snapshot(&json!({"requiresOpenaiAuth":true,"account":{"type":"chatgpt","email":null,"planType":"plus"}})).is_ok());
    assert!(
        QuotaIdentity::read(&usage("acct_123"))
            .unwrap()
            .proof(Instant::now())
            .is_err()
    );
}

#[cfg(unix)]
async fn fixture(
    rows: Vec<Value>,
    expired: bool,
) -> (Result<VerifiedProviderAccount, ProviderError>, Vec<String>) {
    fixture_with_config(rows, expired, json!({"model_provider":"openai","chatgpt_base_url":"https://chatgpt.com/backend-api","model_providers":{}})).await
}
#[cfg(unix)]
async fn fixture_with_config(
    rows: Vec<Value>,
    expired: bool,
    config: Value,
) -> (Result<VerifiedProviderAccount, ProviderError>, Vec<String>) {
    fixture_mode(rows,expired,config,false).await
}
#[cfg(unix)]
async fn fixture_mode(rows:Vec<Value>,expired:bool,config:Value,resource:bool)->(Result<VerifiedProviderAccount,ProviderError>,Vec<String>) {
    use std::os::unix::fs::PermissionsExt;
    let temp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let paths = super::super::paths(
        &root.join("state"),
        "https://server.example/",
        "https://server.example/auth/",
        "test-subject",
        "@owner:example",
    )
    .unwrap();
    let executable = root.join("fake-codex");
    if resource {std::fs::write(paths.home.join("resource-mode"),"").unwrap();}
    std::fs::write(
        paths.home.join("fixture.json"),
        serde_json::to_vec(&rows).unwrap(),
    )
    .unwrap();
    std::fs::write(
        paths.home.join("config.json"),
        serde_json::to_vec(&config).unwrap(),
    )
    .unwrap();
    std::fs::write(&executable, r#"#!/usr/bin/env python3
import json,os,pathlib,sys
home=pathlib.Path.cwd(); rows=json.loads((home/'fixture.json').read_text()); offset=0
assert 'model_provider="openai"' in sys.argv and 'chatgpt_base_url="https://chatgpt.com/backend-api"' in sys.argv
expected=(['account/read','account/rateLimits/read','account/read','account/read','model/list','account/read','account/rateLimits/read','account/read'] if (home/'resource-mode').exists() else ['account/read','account/rateLimits/read','account/read','account/rateLimits/read','account/read'])
for line in sys.stdin:
 r=json.loads(line); m=r.get('method'); i=r.get('id')
 if i is None:
  assert m=='initialized'
  continue
 with (home/'methods.jsonl').open('a') as f:f.write(json.dumps(m)+'\n')
 if m=='initialize':v={'userAgent':'fixture/0.167.0'}
 elif m=='config/read':v={'config':json.loads((home/'config.json').read_text())}
 else:
  assert m==expected[offset], 'identity must not login, fetch credentials, or run a model'
  if m=='account/read':assert r['params']=={'refreshToken':False}
  v=rows[offset]; offset+=1
 print(json.dumps({'id':i,'result':v}),flush=True)
"#).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let result = match Process::spawn_identity(&paths, &executable).await {
        Ok(mut process) => {
            let deadline = if expired {
                Instant::now()
            } else {
                Instant::now() + Duration::from_secs(30)
            };
            let result = if resource {inspect_resource(&mut process,&paths,deadline).await.map(|(proof,_)|proof)} else {observe(&mut process, &paths, deadline).await};
            process.child.kill().await.unwrap();
            result
        }
        Err(error) => Err(error),
    };
    let methods = std::fs::read_to_string(paths.home.join("methods.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(!paths.codex_home.join("auth.json").exists());
    (result, methods)
}
#[cfg(unix)]
#[tokio::test]
async fn identity_reads_only_authenticated_public_metadata_without_login_or_inference() {
    let (result, methods) = fixture(
        vec![
            account("a@example.test"),
            usage("acct_same"),
            account("a@example.test"),
            usage("acct_same"),
            account("a@example.test"),
        ],
        false,
    )
    .await;
    assert!(result.is_ok());
    assert_eq!(
        methods,
        [
            "initialize",
            "config/read",
            "account/read",
            "account/rateLimits/read",
            "account/read",
            "account/rateLimits/read",
            "account/read"
        ]
    );
    let (result, _) = fixture(
        vec![
            account("another-alias@example.test"),
            usage("acct_same"),
            account("another-alias@example.test"),
            usage("acct_same"),
            account("another-alias@example.test"),
        ],
        false,
    )
    .await;
    assert!(result.is_ok());
    // Different home/email does not enter QuotaIdentity or provider proof.
    assert_eq!(
        QuotaIdentity::read(&usage("acct_same")).unwrap().0,
        "acct_same"
    );
}
#[cfg(unix)]
#[tokio::test]
async fn account_switch_or_old_provider_protocol_cannot_produce_capacity_proof() {
    let base = vec![
        account("a@example.test"),
        usage("acct_a"),
        account("a@example.test"),
        usage("acct_a"),
        account("a@example.test"),
    ];
    let mut changed = base.clone();
    changed[3] = usage("acct_b");
    assert!(
        matches!(fixture(changed, false).await.0, Err(error) if error.code == "provider_account_changed")
    );
    let mut changed = base.clone();
    changed[4] = account("b@example.test");
    assert!(
        matches!(fixture(changed, false).await.0, Err(error) if error.code == "provider_account_changed")
    );
    let mut old = base.clone();
    old[1] = json!({"rateLimits":{}});
    assert!(
        matches!(fixture(old, false).await.0, Err(error) if error.code == "provider_identity_unavailable")
    );
    let mut out = base.clone();
    out[2] = json!({"requiresOpenaiAuth":true,"account":null});
    assert!(
        matches!(fixture(out, false).await.0, Err(error) if error.code == "provider_identity_unavailable")
    );
    assert!(fixture(base, true).await.0.is_err());
}

#[test]
fn identity_requires_verified_builtin_provider_origin() {
    let good = json!({"model_provider":"openai","chatgpt_base_url":"https://chatgpt.com/backend-api","model_providers":{}});
    assert!(configuration(&good).is_ok());
    for value in [
        json!({}),
        json!({"model_provider":"openai","chatgpt_base_url":"https://other.example/backend-api"}),
        json!({"model_provider":"custom","chatgpt_base_url":"https://chatgpt.com/backend-api"}),
        json!({"model_provider":"openai","chatgpt_base_url":"https://chatgpt.com/backend-api","model_providers":{"openai":{"base_url":"https://other.example"}}}),
    ] {
        assert!(configuration(&value).is_err());
    }
}

#[cfg(unix)]
#[tokio::test]
async fn startup_rejects_effective_origin_override_before_reading_account_metadata() {
    for config in [
        json!({"model_provider":"openai","chatgpt_base_url":"https://untrusted.example"}),
        json!({"model_provider":"openai","chatgpt_base_url":"https://chatgpt.com/backend-api","model_providers":{"openai":{"base_url":"https://untrusted.example"}}}),
    ] {
        let (result, methods) = fixture_with_config(vec![], false, config).await;
        assert!(matches!(result, Err(error) if error.code == "provider_identity_unavailable"));
        assert_eq!(methods, ["initialize", "config/read"]);
    }
}

#[cfg(unix)]
fn resource_rows()->Vec<Value>{
    vec![account("a@example.test"),usage("quota_original"),account("a@example.test"),account("a@example.test"),
        json!({"data":[{"hidden":false,"id":"coding","model":"gpt-5.6-sol","displayName":"Coding","description":"Public model","isDefault":true,"defaultReasoningEffort":"high","supportedReasoningEfforts":[{"reasoningEffort":"high","description":"High"}]}],"nextCursor":null}),
        account("a@example.test"),usage("quota_original"),account("a@example.test")]
}
#[cfg(unix)]
async fn resource_fixture(rows:Vec<Value>,expired:bool)->(Result<VerifiedProviderAccount,ProviderError>,Vec<String>){
    fixture_mode(rows,expired,json!({"model_provider":"openai","chatgpt_base_url":"https://chatgpt.com/backend-api"}),true).await
}
#[cfg(unix)]
#[tokio::test]
async fn resource_proof_brackets_catalog_with_public_quota_observations(){
    let (result,methods)=resource_fixture(resource_rows(),false).await;
    assert!(result.is_ok());
    assert_eq!(methods,["initialize","config/read","account/read","account/rateLimits/read","account/read","account/read","model/list","account/read","account/rateLimits/read","account/read"]);
}
#[cfg(unix)]
#[tokio::test]
async fn resource_proof_rejects_same_email_quota_switch_and_catalog_account_switch(){
    for (position,value) in [(6,usage("quota_other")),(5,account("other@example.test")),(7,account("other@example.test"))]{
        let mut rows=resource_rows();rows[position]=value;
        assert!(matches!(resource_fixture(rows,false).await.0,Err(e) if e.code=="provider_account_changed"));
    }
}
#[cfg(unix)]
#[tokio::test]
async fn resource_proof_rejects_signed_out_unsupported_identity_and_expiry(){
    let mut rows=resource_rows();rows[0]=json!({"requiresOpenaiAuth":true,"account":null});
    assert!(matches!(resource_fixture(rows,false).await.0,Err(e) if e.code=="provider_connection_required"));
    for position in [1,6] {let mut rows=resource_rows();rows[position]=json!({"rateLimits":{},"email":"a@example.test"});assert!(matches!(resource_fixture(rows,false).await.0,Err(e) if e.code=="provider_identity_unavailable"));}
    assert!(resource_fixture(resource_rows(),true).await.0.is_err());
    let mut rows=resource_rows();rows[4]=json!({"data":[{"id":"incomplete"}],"nextCursor":null});assert!(resource_fixture(rows,false).await.0.is_err());
}

#[test]
fn quota_report_drops_private_fields_and_shares_execution_account_digest() {
    let raw=json!({"accountId":"acct_original","email":"private@example.test","accessToken":"secret",
        "rateLimits":{"primary":{"usedPercent":27,"windowDurationMins":300},"credits":{"balance":"private"}}});
    let report=observed_quota(snapshot(&account("private@example.test")).unwrap(),&raw).unwrap();
    assert_eq!(report.subject_digest,hagency_runtime::codex::session::quota_account_digest("acct_original"));
    let public=serde_json::to_string(&report.limits).unwrap();
    for private in ["private@example.test","secret","acct_original","credits","balance"] {
        assert!(!public.contains(private));
    }
    let unknown=observed_quota(snapshot(&account("private@example.test")).unwrap(),&json!({"rateLimits":{}})).unwrap();
    assert_eq!(unknown.subject_digest,None);
    assert!(unknown.limits.rate_limits.unwrap().primary.is_none());
    assert!(observed_quota(snapshot(&account("same@example.test")).unwrap(),&usage("same@example.test")).is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn idle_quota_probe_only_reads_metadata_and_drains_child_even_on_account_change() {
    use std::os::unix::fs::PermissionsExt;
    for changed in [false,true] {
        let temp=hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let root=temp.path().canonicalize().unwrap();
        let paths=super::super::paths(&root.join("state"),"https://server.example/","https://server.example/auth/","test-subject","@owner:example").unwrap();
        let executable=root.join("quota-fixture");
        let rows=vec![account("a@example.test"),usage("acct_original"),account("a@example.test"),usage(if changed {"acct_replacement"} else {"acct_original"}),account("a@example.test")];
        std::fs::write(paths.home.join("fixture.json"),serde_json::to_vec(&rows).unwrap()).unwrap();
        std::fs::write(&executable,r#"#!/usr/bin/env python3
import json,pathlib,sys
home=pathlib.Path.cwd(); rows=json.loads((home/'fixture.json').read_text()); offset=0
expected=['account/read','account/rateLimits/read','account/read','account/rateLimits/read','account/read']
for line in sys.stdin:
 r=json.loads(line); m=r.get('method'); i=r.get('id')
 if i is None:
  assert m=='initialized'; continue
 with (home/'methods.jsonl').open('a') as f:f.write(json.dumps(m)+'\n')
 if m=='initialize':v={'userAgent':'fixture/0.167.0'}
 elif m=='config/read':v={'config':{'model_provider':'openai','chatgpt_base_url':'https://chatgpt.com/backend-api','model_providers':{}}}
 else:
  assert m==expected[offset], 'idle refresh cannot login or perform inference'
  if m=='account/read':assert r['params']=={'refreshToken':False}
  v=rows[offset]; offset+=1
 print(json.dumps({'id':i,'result':v}),flush=True)
"#).unwrap();
        std::fs::set_permissions(&executable,std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut process=Process::spawn_identity(&paths,&executable).await.unwrap();
        let report=tokio::time::timeout(Duration::from_secs(3),observe_quota(&mut process,&paths,Instant::now()+Duration::from_secs(3))).await.unwrap();
        process.child.kill().await.unwrap();
        if changed {assert!(matches!(report,Err(error) if error.code=="provider_account_changed"));}
        else {assert_eq!(report.unwrap().subject_digest,hagency_runtime::codex::session::quota_account_digest("acct_original"));}
        let methods:Vec<String>=std::fs::read_to_string(paths.home.join("methods.jsonl")).unwrap().lines().map(|line|serde_json::from_str(line).unwrap()).collect();
        assert_eq!(methods,["initialize","config/read","account/read","account/rateLimits/read","account/read","account/rateLimits/read","account/read"]);
        assert!(!paths.codex_home.join("auth.json").exists());
    }
}
