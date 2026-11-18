//! New daemon/OwnerRuntime lifecycle, using the NativeOwner OAuth fixture.
//! This is NOT encrypted Agent execution evidence: no Agent turn is launched.
use super::*;
use hagency::daemon::{self, bridge::ControllerClient, enrollment::Enrollment, host};
use hagency::native_owner::{
    Command as NativeCommand, MatrixAccessToken, MatrixTokenSource, NativeError, NativeOwner,
};
use std::{
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

const OWNER: &str = "@owner:example.test";
const TOKEN: &str = "daemon-fixture-access-token";
const CLIENT: &str = "fixture-public-client";
const DEVICE: &str = "OWNER_DAEMON_FIXTURE";

struct Source;
impl MatrixTokenSource for Source {
    fn access_token(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<MatrixAccessToken, NativeError>> + Send + '_>,
    > {
        Box::pin(async {
            Ok(MatrixAccessToken {
                access_token: TOKEN.into(),
                client_id: CLIENT.into(),
            })
        })
    }
}

#[derive(Clone)]
struct MatrixApi {
    fixture: OAuthFixture,
    whoami: Arc<AtomicU64>,
}
#[handler]
impl MatrixApi {
    async fn handle(
        &self,
        req: &mut Request,
        depot: &mut Depot,
        res: &mut Response,
        ctrl: &mut FlowCtrl,
    ) {
        let path = req.uri().path();
        let base = format!("{}/", self.fixture.origin);
        if path.ends_with("/account_data/m.secret_storage.default_key") {
            res.status_code(StatusCode::NOT_FOUND);
            res.render(Json(json!({"errcode":"M_NOT_FOUND","error":"fixture has no secret-storage default key"})));
            ctrl.skip_rest();
            return;
        }
        let value = if path.ends_with("/auth_metadata") {
            Some(
                json!({"issuer":format!("{base}auth/"),"authorization_endpoint":format!("{base}auth/oauth2/authorize"),"token_endpoint":format!("{base}auth/oauth2/token"),"revocation_endpoint":format!("{base}auth/oauth2/revoke"),"response_types_supported":["code"],"response_modes_supported":["query","fragment"],"grant_types_supported":["authorization_code","refresh_token"],"code_challenge_methods_supported":["S256"]}),
            )
        } else if path.ends_with("/account/whoami") {
            assert_eq!(
                req.headers().get("authorization").unwrap(),
                format!("Bearer {TOKEN}").as_str()
            );
            self.whoami.fetch_add(1, Ordering::AcqRel);
            Some(json!({"user_id":OWNER,"device_id":DEVICE}))
        } else if path.ends_with("/versions") {
            Some(json!({"versions":["v1.15"]}))
        } else if path == "/hagency/api/v1/devices/current" {
            assert_eq!(
                req.headers().get("authorization").unwrap(),
                format!("Bearer {}", "e".repeat(64)).as_str()
            );
            Some(
                json!({"userId":"stable-user-id","mxid":OWNER,"subject":OWNER,"clientId":CLIENT,"issuer":format!("{base}auth/"),"deviceId":"dev_fixture","generation":1,"validUntilMs":until()}),
            )
        } else if matches!(
            path,
            "/hagency/api/v1/management/catalog"
                | "/hagency/api/v1/management/pool-agent-requests/query"
        ) {
            Some(json!({"items":[],"nextAfter":null}))
        } else if path == "/hagency/api/v1/management/shared-execution/approvals/query" {
            Some(json!({"approvals":[]}))
        } else {
            None
        };
        if let Some(value) = value {
            res.render(Json(value));
            ctrl.skip_rest();
        } else {
            ctrl.call_next(req, depot, res).await;
        }
    }
}

async fn wait_running(home: &Path) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut last = None;
    loop {
        if let host::Presence::Running(hello) = host::probe(home).await {
            if hello.hosting == hagency_metering::dashboard::HostingState::Running {
                return;
            }
            let mut client = daemon::DaemonClient::connect(home).await.unwrap();
            last = Some(
                client
                    .call(&daemon::Request::new(
                        daemon::new_request_id(),
                        Some(OWNER.into()),
                        daemon::Command::Snapshot {
                            period: None,
                            usage_filter: None,
                        },
                    ))
                    .await
                    .unwrap(),
            );
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "enrolled OwnerRuntime failed to run: {last:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
fn launch(
    home: &Path,
) -> (
    hagency_matrix::CancellationToken,
    tokio::task::JoinHandle<Result<(), host::HostError>>,
) {
    let stop = hagency_matrix::CancellationToken::new();
    let token = stop.clone();
    let path = home.to_path_buf();
    (
        stop,
        tokio::spawn(async move { host::run(&path, token).await }),
    )
}

#[tokio::test]
async fn enrolled_owner_runtime_survives_two_controller_detaches_and_restart() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let fake = OAuthFixture {
        origin: format!("http://{address}"),
        state: Arc::new(Mutex::new(OAuthState {
            owner: OWNER.into(),
            active: true,
            challenge: None,
            enrollments: 0,
            lifetime: 900,
            refresh: false,
            refreshes: 0,
            revocations: 0,
            renewals: 0,
            token: TOKEN.into(),
            issuer_override: None,
            revoke_unavailable: false,
            binding_state: "joining".into(),
            agent_state: "creating".into(),
            management_calls: 0,
            commands: Default::default(),
            execution_device: Some("dev_fixture".into()),
            agent_generation: 1,
            direct_room: None,
            direct_mapping: None,
            direct_index: json!({}),
            direct_creates: 0,
            direct_unknown: false,
            direct_deny: false,
            direct_foreign: false,
            direct_journal: None,
            direct_rejoin: Rejoin::default(),
        })),
    };
    let whoami = Arc::new(AtomicU64::new(0));
    let service = Service::new(
        Router::new()
            .hoop(fake.clone())
            .hoop(MatrixApi {
                fixture: fake.clone(),
                whoami: whoami.clone(),
            })
            .push(Router::with_path("{**path}").goal(oauth)),
    );
    let acceptor = salvo::conn::TcpListener::new(address).bind().await;
    let server = tokio::spawn(async move {
        Server::new(acceptor).serve(service).await;
    });
    let owner_home = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let root = owner_home.path().join("owner");
    let origin = format!("{}/", fake.origin);
    // The original installed native owner enrolls through production gates.
    // This is a declared isolated OAuth server, not an execution-device bypass.
    let original = NativeOwner::open_with_matrix(&root, &origin, OWNER, Arc::new(Source))
        .await
        .unwrap();
    assert!(original.execute(NativeCommand::Agents).await.unwrap()["agents"].is_array());
    original.suspend().await.unwrap();
    drop(original);
    assert_eq!(fake.state.lock().unwrap().enrollments, 1);

    let home = tempfile::Builder::new()
        .prefix("hag-daemon-")
        .tempdir_in("/tmp")
        .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    hagency_store::private::directory(home.path()).unwrap();
    let matrix = home.path().join(daemon::MATRIX);
    hagency_store::private::directory(&matrix).unwrap();
    // The saved SDK session is fixture input, never borrowed from a UI SDK.
    hagency_store::private::write_new(&matrix.join("session.json"),&serde_json::to_vec(&json!({"version":1,"homeserver":origin,"owner":OWNER,"oauthClientId":CLIENT,"user":{"user_id":OWNER,"device_id":DEVICE,"access_token":TOKEN}})).unwrap()).unwrap();
    let enrollment = Enrollment::enroll(home.path(), &origin, OWNER, None, &root).unwrap();
    let (stop, task) = launch(home.path());
    wait_running(home.path()).await;
    let a = ControllerClient::new(home.path().into(), OWNER.into(), origin.clone(), 1);
    let b = ControllerClient::new(home.path().into(), OWNER.into(), origin.clone(), 2);
    for controller in [&a, &b] {
        assert!(controller.execute(NativeCommand::Agents).await.unwrap()["agents"].is_array());
    }
    assert!(
        NativeOwner::open_with_matrix(&root, &origin, OWNER, Arc::new(Source))
            .await
            .is_err(),
        "daemon must retain exclusive original root custody"
    );
    a.retire();
    b.retire();
    drop(a);
    drop(b);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !task.is_finished(),
        "detaching both controllers must not stop OwnerRuntime"
    );
    let detached_reads = whoami.load(Ordering::Acquire);
    tokio::time::timeout(Duration::from_secs(20), async {
        while whoami.load(Ordering::Acquire) == detached_reads {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("background OwnerRuntime must authenticate without either controller attached");
    wait_running(home.path()).await;
    assert!(
        whoami.load(Ordering::Acquire) > 0,
        "the daemon's independent SDK must actually authenticate"
    );
    let key = std::fs::read(matrix.join("store-key")).unwrap();
    let session = std::fs::read(matrix.join("session.json")).unwrap();
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(15), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(
        host::probe(home.path()).await,
        host::Presence::NotRunning
    ));
    assert_eq!(
        Enrollment::load(home.path()).unwrap().unwrap().owner_root,
        enrollment.owner_root
    );

    let reads = whoami.load(Ordering::Acquire);
    let (stop, task) = launch(home.path());
    wait_running(home.path()).await;
    let restored = ControllerClient::new(home.path().into(), OWNER.into(), origin.clone(), 3);
    assert!(restored.execute(NativeCommand::Agents).await.unwrap()["agents"].is_array());
    assert!(
        whoami.load(Ordering::Acquire) > reads,
        "restart must use restored independent SDK custody"
    );
    assert_eq!(
        fake.state.lock().unwrap().enrollments,
        1,
        "restore must not mint a duplicate execution-device enrollment"
    );
    assert_eq!(std::fs::read(matrix.join("store-key")).unwrap(), key);
    assert_eq!(std::fs::read(matrix.join("session.json")).unwrap(), session);
    restored.retire();
    drop(restored);
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(15), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    server.abort();
}
