use crate::console::native::{ManagementPackage, NativeError, NativeOwner, failure};
use crate::console::owned_agents::profile_identity;
use hagency_store::management::Permission;
use std::{path::PathBuf,sync::{Arc,atomic::Ordering},time::{Duration,Instant}};
use hagency_core::project::Resource;
use serde_json::{Value, json};
use std::{
    fs,
    sync::atomic::{AtomicBool, AtomicUsize},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Source {
    live: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
}
impl super::super::MatrixTokenSource for Source {
    fn access_token(
        &self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<super::super::MatrixAccessToken, NativeError>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async {
            self.calls.fetch_add(1, Ordering::AcqRel);
            if !self.live.load(Ordering::Acquire) {
                return Err(failure(401, "matrix_account_changed"));
            }
            Ok(super::super::MatrixAccessToken {
                access_token: "fixture-sdk".into(),
                client_id: "sdk-client".into(),
            })
        })
    }
}
static TEST_SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
pub(in crate::console::native) struct Fixture {
    _serial: tokio::sync::MutexGuard<'static, ()>,
    pub(in crate::console::native) temp: tempfile::TempDir,
    pub(in crate::console::native) owner: NativeOwner,
    #[cfg(feature = "resource-adoption")]
    pub(in crate::console::native) recipient_routes: Arc<std::sync::Mutex<std::collections::BTreeMap<String,Value>>>,
    #[cfg(feature = "resource-adoption")]
    pub(in crate::console::native) recipient_requests: Arc<std::sync::Mutex<Vec<String>>>,
    origin: String,
    profile: String,
    pub(in crate::console::native) live: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
    network: Arc<AtomicUsize>,
    server: tokio::task::JoinHandle<()>,
}
impl Fixture {
    pub(in crate::console::native) async fn new() -> Self { Self::with_installation(false).await }
    #[cfg(feature = "resource-adoption")]
    pub(in crate::console::native) async fn with_configured_installation() -> Self { Self::with_installation(true).await }
    async fn with_installation(configured: bool) -> Self {
        let serial = TEST_SERIAL.lock().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        let server_origin = origin.clone();
        let network = Arc::new(AtomicUsize::new(0));
        let count = network.clone();
        let recipient_routes = Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::<String,Value>::new()));
        let recipient_requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let routes = recipient_routes.clone();
        let requests = recipient_requests.clone();
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let origin = server_origin.clone();
                let count = count.clone();
                let routes = routes.clone();
                let requests = requests.clone();
                tokio::spawn(async move {
                    let mut raw = Vec::new();
                    let mut part = [0; 4096];
                    let end = loop {
                        let n = stream.read(&mut part).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        raw.extend_from_slice(&part[..n]);
                        assert!(raw.len() < 32768);
                        if let Some(i) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                            break i + 4;
                        }
                    };
                    let head = String::from_utf8_lossy(&raw[..end]).to_string();
                    let length = head
                        .lines()
                        .find_map(|line| {
                            line.split_once(':')
                                .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                                .map(|(_, v)| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    while raw.len() < end + length {
                        let n = stream.read(&mut part).await.unwrap();
                        if n == 0 {
                            return;
                        }
                        raw.extend_from_slice(&part[..n]);
                    }
                    let path = head
                        .lines()
                        .next()
                        .unwrap()
                        .split_whitespace()
                        .nth(1)
                        .unwrap();
                    count.fetch_add(1, Ordering::AcqRel);
                    let at = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_millis() as u64;
                    requests.lock().unwrap().push(head.lines().next().unwrap().to_owned());
                    let decoded = percent_encoding::percent_decode_str(path).decode_utf8().unwrap().into_owned();
                    let fixture_reply = routes.lock().unwrap().get(&decoded).cloned();
                    let value = if let Some(value) = fixture_reply { value } else { match path {
                        "/hagency/api/v1/discovery" => {
                            json!({"product":"hagency-server","version":"0.1.0","protocolVersion":3,"capabilities":["pasion-oauth","owner-agent-appservice-v1","global-agent-identity-v2","execution-device-v1","owner-direct-v1"],"homeserver":origin,"issuer":format!("{origin}auth/")})
                        }
                        "/hagency/api/v1/sessions/pasion" => {
                            json!({"token":"a".repeat(64),"userId":"uid","mxid":"@owner:test","validUntilMs":at+30000})
                        }
                        "/hagency/api/v1/identity" => {
                            json!({"userId":"uid","mxid":"@owner:test","issuer":format!("{origin}auth/"),"subject":"source-subject","clientId":"sdk-client","validUntilMs":at+30000})
                        }
                        "/hagency/api/v1/devices" => {
                            json!({"deviceId":"fixture-device","token":"b".repeat(64),"generation":1,"validUntilMs":at+30000})
                        }
                        "/hagency/api/v1/sessions/current/renew" => {
                            json!({"validUntilMs":at+30000})
                        }
                        "/hagency/api/v1/devices/current" => {
                            json!({"userId":"uid","mxid":"@owner:test","issuer":format!("{origin}auth/"),"subject":"source-subject","clientId":"sdk-client","deviceId":"fixture-device","generation":1,"validUntilMs":at+30000})
                        }
                        "/hagency/api/v1/sessions/current" => json!({}),
                        "/hagency/api/v1/agents" => json!({"agents":[]}),
                        _ => panic!("unexpected fixture route: {path}"),
                    }};
                    let body = value.to_string();
                    let status = value["__fixture_http_status"].as_u64().unwrap_or(200);
                    let _=stream.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await;
                });
            }
        });
        let temp = if configured {
            hagency_platform::legacy_writer::test_support::configured_root().unwrap();
            tempfile::tempdir().unwrap()
        } else { hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap() };
        let live = Arc::new(AtomicBool::new(true));
        let calls = Arc::new(AtomicUsize::new(0));
        let owner = NativeOwner::open_with_matrix(
            &temp.path().join("host"),
            &origin,
            "@owner:test",
            Arc::new(Source {
                live: live.clone(),
                calls: calls.clone(),
            }),
        )
        .await
        .unwrap();
        // Isolate inspection from the owner's independently running legacy
        // configuration worker; this preserves real SDK/device authorization.
        let tasks = owner
            .0
            .configuration_tasks
            .lock()
            .unwrap()
            .drain(..)
            .collect::<Vec<_>>();
        for task in &tasks {
            task.abort();
        }
        for task in tasks {
            let _ = task.await;
        }
        let profile = profile_identity(
            &origin,
            &format!("{origin}auth/"),
            "source-subject",
            "@owner:test",
        )
        .unwrap();
        Self {
            _serial: serial,
            temp,
            owner,
            origin,
            profile,
            live,
            calls,
            network,
            #[cfg(feature = "resource-adoption")]
            recipient_routes,
            #[cfg(feature = "resource-adoption")]
            recipient_requests,
            server,
        }
    }
    fn root(&self) -> PathBuf {
        self.owner.0.host.state_directory().into()
    }
    fn owners(&self) -> PathBuf {
        let path = self.root().join("owned-agent-owners");
        hagency_store::private::directory(&path).unwrap();
        path
    }
    pub(in crate::console::native) fn domain(
        &self,
    ) -> (hagency_store::DomainRepository, PathBuf, Resource) {
        let path = self.owners().join(format!("management_{}", self.profile));
        let mut db = hagency_store::DomainRepository::open_managed(&path, &self.profile).unwrap();
        let resource:Resource=serde_json::from_value(json!({"presetId":"original-preset","seatId":"original-seat","framework":"codex","model":"gpt-5.6-sol","reasoning":"medium","published":false,"ceiling":{"tokens":5000,"period":"monthly"}})).unwrap();
        db.put_resource(&resource).unwrap();
        (db, path, resource)
    }
    pub(in crate::console::native) fn personal(&self) -> (hagency_agent_local::Ledger, PathBuf) {
        let dir = self.owners().join(format!("owner_{}", self.profile));
        hagency_store::private::directory(&dir).unwrap();
        hagency_store::private::replace(&dir.join("profile-identity.json"), &serde_json::to_vec(&json!({"version":1,"origin":self.origin,"issuer":format!("{}auth/",self.origin),"subject":"source-subject","owner":"@owner:test"})).unwrap()).unwrap();
        let path = dir.join("agent-local.sqlite");
        hagency_store::private::open(&path, true).unwrap();
        let ledger =
            hagency_agent_local::Ledger::open_scoped(&path, "@owner:test", &self.profile).unwrap();
        (ledger, path)
    }
    pub(in crate::console::native) async fn stop(self) {
        self.owner.shutdown().await;
        self.server.abort();
    }
}
fn package() -> ManagementPackage {
    ManagementPackage {
        digest: "a".repeat(64),
        instance: 1,
        account_epoch: 1,
        permissions: vec![Permission::ReadResources],
    }
}#[cfg(feature = "resource-adoption")]
impl Fixture {
    pub(in crate::console::native) async fn initialize_current(self,registry:hagency_store::owner_resources::OwnerResourceRegistry)->Self {
        use hagency_store::owner_resources::{ProfileAuthority,adoption::ProfileInitializationAuthority};
        let device=super::super::device_setup::source(&self.owner).await.unwrap();
        let mut p=package();p.permissions.push(Permission::PrepareSourceQuiescence);
        let manager=self.owner.management(p).await.unwrap();
        let journal=manager.current_setup_journal().await.unwrap();
        let until=Instant::now()+Duration::from_secs(30);
        let authority=ProfileInitializationAuthority::new(registry.clone(),ProfileAuthority::new(self.profile.clone(),device.owner_mxid().into(),until).unwrap(),until,||true).unwrap();
        registry.initialize_current_profile(authority,"current_fixture".into(),journal).await.unwrap();
        let selected=super::super::CurrentProfileSelection::completed(&self.owner,registry,"current_fixture".into(),self.profile.clone(),device.device_id().into());
        selected.persist(&self.owner).unwrap();
        manager.retire();
        drop(manager);
        self.restart_onboarding(selected).await
    }
    pub(in crate::console::native) async fn current_resource(&self,registry:&hagency_store::owner_resources::OwnerResourceRegistry)->hagency_core::project::Resource {
        use hagency_store::owner_resources::{ProfileAuthority,VerifiedProviderAccount};
        let until=Instant::now()+Duration::from_secs(30);
        let profile=ProfileAuthority::new(self.profile.clone(),"@owner:test".into(),until).unwrap();
        let proof=VerifiedProviderAccount::new("codex","https://provider.test","current-fixture",until).unwrap();
        let receipt=registry.prepare_enrollment(profile,"current_resource".into(),proof,"gpt-5.6-sol".into(),Some("medium".into()),700,until).unwrap().approve().await.unwrap();
        registry.with_repository_for_tests(move|db|db.resource_configuration(&receipt.resource_id)).await.unwrap()
    }
    pub(in crate::console::native) async fn restart_onboarding(self,selection:super::super::CurrentProfileSelection)->Self {
        let Self {_serial,temp,owner,origin,profile,live,calls,network,recipient_routes,recipient_requests,server}=self;
        let state=owner.0.host.state_directory().to_owned();
        let source=owner.0.matrix_source.lock().unwrap().as_ref().unwrap().clone();
        owner.suspend().await.unwrap();drop(owner);
        let owner=selection.open_with_matrix_guarded(&state,&origin,"@owner:test",source,|_|Ok(())).await.unwrap();
        Self {_serial,temp,owner,origin,profile,live,calls,network,recipient_routes,recipient_requests,server}
    }
    pub(in crate::console::native) async fn restart_current(self,registry:hagency_store::owner_resources::OwnerResourceRegistry,initialization_id:String)->Self {
        let Self {_serial,temp,owner,origin,profile,live,calls,network,recipient_routes,recipient_requests,server}=self;
        let state=owner.0.host.state_directory().to_owned();
        let source=owner.0.matrix_source.lock().unwrap().as_ref().unwrap().clone();
        owner.suspend().await.unwrap();drop(owner);
        let missing=NativeOwner::open_current_profile_with_matrix_guarded(&state,&origin,"@owner:test",source.clone(),registry.clone(),"missing-current".into(),|_|Ok(())).await;
        assert!(missing.is_err());drop(missing);
        let owner=NativeOwner::open_current_profile_with_matrix_guarded(&state,&origin,"@owner:test",source,registry,initialization_id,|_|Ok(())).await.unwrap();
        assert_eq!(owner.0.configuration_tasks.lock().unwrap().len(),1);
        Self {_serial,temp,owner,origin,profile,live,calls,network,recipient_routes,recipient_requests,server}
    }
}
