//! Isolated real pooled Agent + Server/Pasion/Palpo + daemon host subprocess + Matrix SDK E2EE.
//! Run only with the smoke harness's private loopback fixture. The provider is
//! an explicit deterministic subprocess; no production login or inference.
#![recursion_limit = "512"]

#[cfg(unix)]
mod fixture {
    use hagency::daemon::{self, bridge::ControllerClient, enrollment::Enrollment, host};
    use hagency::native_owner::{MatrixAccessToken, MatrixTokenSource, NativeError, NativeOwner};
    use matrix_sdk::ruma::{
        OwnedRoomId, OwnedUserId,
        events::{
            Mentions,
            room::message::{OriginalSyncRoomMessageEvent, RoomMessageEventContent},
        },
    };
    use matrix_sdk::{
        Client, SessionMeta, SessionTokens, authentication::matrix::MatrixSession,
        config::SyncSettings, store::RoomLoadSettings,
    };
    use serde_json::{Value, json};
    use std::{
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        sync::{Arc, Mutex},
        time::Duration,
    };

    const ANSWER: &str = "OFFLINE_ENCRYPTED_DAEMON_ANSWER";
    struct Source(Value);
    impl MatrixTokenSource for Source {
        fn access_token(
            &self,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<MatrixAccessToken, NativeError>>
                    + Send
                    + '_,
            >,
        > {
            Box::pin(async {
                Ok(MatrixAccessToken {
                    access_token: self.0["accessToken"].as_str().unwrap().into(),
                    client_id: self.0["clientId"].as_str().unwrap().into(),
                })
            })
        }
    }
    async fn http(
        base: &str,
        token: &str,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Value {
        let mut request = reqwest::Client::new()
            .request(method, format!("{base}{path}"))
            .bearer_auth(token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let reply = request.send().await.unwrap();
        let status = reply.status();
        let value: Value = reply.json().await.unwrap();
        assert!(
            status.is_success(),
            "fixture request {path}: {status}: {value}"
        );
        value
    }
    // Room administration is a Server owner operation. The adopted executor's
    // legacy source root deliberately cannot perform personal-owner mutations.
    async fn owner_api(
        base: &str,
        matrix_token: &str,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Value {
        let session = http(
            base,
            matrix_token,
            reqwest::Method::POST,
            "/hagency/api/v1/sessions/pasion",
            Some(json!({"accessToken":matrix_token})),
        )
        .await;
        http(base, session["token"].as_str().unwrap(), method, path, body).await
    }
    fn segment(value: &str) -> String {
        percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
    }
    async fn wait_running(home: &Path) {
        let limit = tokio::time::Instant::now() + Duration::from_secs(45);
        loop {
            if let host::Presence::Running(v) = host::probe(home).await {
                if v.hosting == hagency_metering::dashboard::HostingState::Running {
                    return;
                }
            }
            assert!(
                tokio::time::Instant::now() < limit,
                "daemon did not reach Running"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    struct DaemonProcess(tokio::process::Child);
    impl DaemonProcess {
        async fn start(home: &Path, log: &Path) -> Self {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log)
                .unwrap();
            let child = tokio::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "isolated_pooled_daemon_process",
                    "--ignored",
                    "--nocapture",
                ])
                .env("HAGENCY_DAEMON_CRYPTO_HOME", home)
                .env("RUST_LOG", "hagency=debug,matrix_sdk=warn")
                .stdout(file.try_clone().unwrap())
                .stderr(file)
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            let process = Self(child);
            wait_running(home).await;
            process
        }
        async fn stop(mut self) {
            // Signal only this test's owned child. Production shutdown retains
            // custody through drain; no daemon service or other process is touched.
            let pid = self.0.id().unwrap();
            assert!(
                std::process::Command::new("/bin/kill")
                    .args(["-TERM", &pid.to_string()])
                    .status()
                    .unwrap()
                    .success()
            );
            assert!(
                tokio::time::timeout(Duration::from_secs(30), self.0.wait())
                    .await
                    .unwrap()
                    .unwrap()
                    .success()
            );
        }
    }
    fn turns(marker: &Path) -> Vec<Value> {
        std::fs::read_to_string(marker)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    async fn reply_count(
        replies: &Arc<Mutex<Vec<(String, String)>>>,
        marker: &Path,
        expected: usize,
    ) {
        let limit = tokio::time::Instant::now() + Duration::from_secs(100);
        loop {
            if replies.lock().unwrap().len() >= expected {
                break;
            }
            assert!(
                tokio::time::Instant::now() < limit,
                "encrypted replies timed out; actual model turns={} replies={:?}",
                turns(marker).len(),
                replies.lock().unwrap()
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        assert_eq!(
            turns(marker).len(),
            expected,
            "one model execution per accepted encrypted request"
        );
    }
    async fn send_expected(
        client: &Client,
        room: &str,
        text: &str,
        mention: Option<&str>,
        expected: &mut Vec<(String, String)>,
    ) -> String {
        let event = send(client, room, text, mention).await;
        expected.push((room.to_owned(), event.clone()));
        event
    }
    fn reply_parent(content: &RoomMessageEventContent) -> String {
        let value = serde_json::to_value(content).unwrap();
        value["m.relates_to"]["m.in_reply_to"]["event_id"]
            .as_str()
            .or_else(|| value["m.relates_to"]["event_id"].as_str())
            .unwrap_or("")
            .to_owned()
    }
    async fn send(client: &Client, room: &str, text: &str, mention: Option<&str>) -> String {
        let room: OwnedRoomId = room.try_into().unwrap();
        let room = client.get_room(&room).expect("human joined room");
        assert!(room.latest_encryption_state().await.unwrap().is_encrypted());
        let mut content = RoomMessageEventContent::text_plain(text);
        if let Some(agent) = mention {
            content.mentions = Some(Mentions::with_user_ids([
                OwnedUserId::try_from(agent).unwrap()
            ]));
        }
        room.send(content)
            .await
            .unwrap()
            .response
            .event_id
            .to_string()
    }

    use hagency::native_owner::Command;
    use hagency::native_owner::{
        NamespaceEnrollment, ManagementCommand, ManagementPackage, NativeManagement, OwnerGrantChoices,
        OwnerGrantPublicationResult, initialize_resource_activation_fixture,
    };
    use hagency_store::management::{Permission, ResourceQuery, PrepareAccount, AccountInventoryQuery, AccountModelQuery};
    fn package(instance: u64) -> ManagementPackage {
        use Permission::*;
        ManagementPackage {
            digest: "a".repeat(64),
            instance,
            account_epoch: 1,
            permissions: vec![
                ReadResources,
                ReadAccounts,
                PrepareAccounts,
                ReadAccountModels,
                EnrollResources,
                PrepareSourceQuiescence,
                ReadConnections,
                ReadCatalog,
                ReadInbox,
                DecideInbox,
                ContributeResources,
                ReadIntents,
                ConsentGrants,
                PublishGrants,
                VerifyConnections,
                ResumeConnections,
                ReadAgentRequests,
                RequestAgents,
                DecideAgentRequests,
                ReadAgentStatus,
                ReadAgentWork,
                ReadAgentBudget,
                ReadAgentRunControl,
                ChangeAgentRunControl,
                ReadAgentSettlement,
                ReadExecutionPolicy,
            ],
        }
    }
    fn unique_id(label: &str) -> String {
        let root = std::env::var("HAGENCY_DAEMON_CRYPTO_ROOT").unwrap();
        let tag = Path::new(&root)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>();
        format!("{label}_{tag}")
    }
    async fn central(manager: &NativeManagement, mut value: Value) -> Value {
        if let Some(id) = value["command_id"].as_str() {
            value["command_id"] = json!(unique_id(id));
        }
        let operation = value["operation"].as_str().unwrap().to_owned();
        manager
            .central(serde_json::from_value(value).unwrap())
            .await
            .unwrap_or_else(|e| panic!("native central {operation}: {e:?}"))
    }
    fn operation(value: Value) -> ManagementCommand {
        serde_json::from_value(value).unwrap()
    }
    fn stage(label: &str) {
        println!("POOLED STAGE: {label}");
    }

    fn real_pooled_daemon_encrypted_restart() {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let provider = root.path().join("offline-provider");
        let marker = root.path().join("turns.jsonl");
        let script = include_str!("fixtures/pooled_daemon_provider.py").replace(
            "'__FIXTURE_MARKER__'",
            &serde_json::to_string(&marker).unwrap(),
        );
        std::fs::write(&provider, script).unwrap();
        std::fs::set_permissions(&provider, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(&marker, "").unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "real_pooled_daemon_encrypted_child",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("HAGENCY_DAEMON_CRYPTO_ROOT", root.path())
            .env("HAGENCY_DAEMON_CRYPTO_TURNS", &marker)
            .env("HAGENCY_CODEX_BINARY", &provider)
            .env("HAGENCY_CLAUDE_BINARY", root.path().join("no-claude"))
            .env("HAGENCY_OCTOS_BINARY", root.path().join("no-octos"))
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .status()
            .unwrap();
        if !result.success() {
            let path = root.keep();
            panic!(
                "isolated pooled fixture failed (diagnostics: {})",
                path.display()
            );
        }
    }

    async fn real_pooled_daemon_encrypted_child() {
        let root = PathBuf::from(std::env::var_os("HAGENCY_DAEMON_CRYPTO_ROOT").unwrap())
            .canonicalize()
            .unwrap();
        initialize_resource_activation_fixture(&root).unwrap();
        let fixture_path =
            PathBuf::from(std::env::var_os("HAGENCY_DAEMON_CRYPTO_FIXTURE").unwrap());
        let fixture: Value =
            serde_json::from_reader(hagency_store::private::open(&fixture_path, false).unwrap())
                .unwrap();
        let native_admin = std::env::var_os("HAGENCY_POOLED_NATIVE_ADMIN_ONLY").is_some();
        let base = fixture["base"].as_str().unwrap();
        let origin = format!("{}/", base.trim_end_matches('/'));
        let url = reqwest::Url::parse(base).unwrap();
        assert_eq!(url.scheme(), "http");
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        let owner = fixture["daemon"]["owner"].as_str().unwrap();
        assert_eq!(fixture["human"]["owner"], owner);
        let daemon_token = fixture["daemon"]["accessToken"].as_str().unwrap();
        let human_token = fixture["human"]["accessToken"].as_str().unwrap();
        let member_owner = fixture["member"]["owner"]
            .as_str()
            .expect("separate real human");
        let member_token = fixture["member"]["accessToken"].as_str().unwrap();
        assert_ne!(member_owner, owner);
        let marker = PathBuf::from(std::env::var_os("HAGENCY_DAEMON_CRYPTO_TURNS").unwrap());
        let native_root = root.join("owner");
        let source = Arc::new(Source(fixture["daemon"].clone()));
        let native = NativeOwner::open_with_matrix(&native_root, &origin, owner, source.clone())
            .await
            .unwrap();
        stage("authenticated current resource owner");
        let review=native.review_device_setup(package(1),None,None).await.unwrap();
        let selection=native.confirm_device_setup(review,package(1),||true).await.unwrap();
        native.suspend().await.unwrap();
        drop(native);
        let native = selection
            .open_with_matrix_guarded(&native_root, &origin, owner, source, |_| Ok(()))
            .await
            .unwrap();
        assert_eq!(
            selection.device_setup_phase(&native).await.unwrap(),
            "active"
        );
        stage("current canonical profile active");
        let manager = native.management(package(3)).await.unwrap();
        let account=manager.prepare_account(PrepareAccount{command_id:unique_id("fixture_account")}).await.unwrap();
        let inventory=manager.account_inventory(AccountInventoryQuery{id:Some(account.account_id.clone()),after:None}).await.unwrap();
        let selected=inventory.account.unwrap();
        let receipt=manager.enroll_account_resource(NamespaceEnrollment{
            command_id:unique_id("fixture_enroll"),
            account:AccountModelQuery{account_id:account.account_id,expected_revision:selected.revision.unwrap()},
            model:"gpt-5.6-sol".into(),reasoning:Some("medium".into()),monthly_tokens:10000,
        },std::time::Instant::now()+Duration::from_secs(30)).await.unwrap();
        let resource_id=receipt.resource_id;

        let resources = manager.resources(ResourceQuery::default()).await.unwrap();
        let resource = resources
            .resources
            .iter()
            .find(|r| r.id == resource_id)
            .unwrap();
        let contribution=central(&manager,json!({"operation":"contribute","publication_first":true,"command_id":"fixture_contribute","resource_id":resource.id,
        "expected_revision":resource.revision,"label":"Offline pooled daemon acceptance","coordinator_mxid":owner})).await;
        let admission = &contribution["admission"];
        let contribution_id = admission["contribution"]["id"]
            .as_str()
            .expect("contribution id")
            .to_owned();
        assert_eq!(admission["contribution"]["state"], "draft");
        let before = central(&manager, json!({"operation":"pools","after":null,"capability":null})).await;
        assert!(!before["items"].as_array().unwrap().iter().any(|p| p["id"] == contribution_id));
        let access = selection.grant_access(&native, package(4)).await.unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        // The fixture explicitly selects a UTC monthly pool calendar and
        // capacity within the current resource ceiling.
        let review = access
            .prepare(
                contribution_id.clone(),
                OwnerGrantChoices {
                    renewal_mode: None, per_request_tokens: None,
                    calendar: Some(hagency_core::recurring_quota::Calendar {
                        cadence: hagency_core::recurring_quota::Cadence::Monthly,
                        time_zone: "UTC".into(),
                    }),
                    coordinator_mxid: owner.into(),
                    delegation_expires_at_ms: now + 3_600_000,
                    allow_self_approval: true,
                    eligible_managers: vec![owner.into()],
                    allocated_tokens: 5000,
                },
            )
            .await
            .unwrap();
        let progress = access.confirm(review).await.unwrap();
        let published = match access
            .review_publication(progress.summary.intent_id.clone())
            .await
            .unwrap()
        {
            OwnerGrantPublicationResult::Review(review) => {
                access.confirm_publication(*review).await.unwrap()
            }
            OwnerGrantPublicationResult::Progress(progress) => *progress,
        };
        assert!(matches!(
            published.state,
            hagency::native_owner::OwnerGrantPublicationState::ServerObserved
        ));
        let submitted = central(&manager, json!({"operation":"connection","id":contribution_id})).await;
        assert_eq!(submitted["admission"]["contribution"]["state"], "pending_admission");
        let pending_pools = central(&manager, json!({"operation":"pools","after":null,"capability":null})).await;
        assert!(!pending_pools["items"].as_array().unwrap().iter().any(|p| p["id"] == contribution_id));
        central(&manager,json!({"operation":"decide","command_id":"fixture_admit","request_id":admission["request"]["id"],
            "expected_revision":submitted["admission"]["request"]["revision"],"approve":true})).await;
        stage("one native publication before approval; approval exposes the original capacity without another owner command");
        let pool_page = central(
            &manager,
            json!({"operation":"pools","after":null,"capability":null}),
        )
        .await;
        let pool = pool_page["items"]
            .as_array()
            .expect("pool items")
            .iter()
            .find(|p| p["id"] == contribution_id)
            .expect("published pool");
        assert_eq!(pool["canRequest"], true, "{pool_page}");
        let room = manager
            .pool_agent_room(
                unique_id("fixture_room"),
                "Encrypted pooled daemon fixture".into(),
            )
            .await
            .unwrap();
        assert_eq!(
            room.creation.phase,
            hagency::native_owner::MatrixCreationPhase::Complete,
            "{room:?}"
        );
        let dm = room
            .creation
            .room_id
            .as_deref()
            .expect("native pooled room");
        let request=central(&manager,json!({"operation":"pool_agent_request","command_id":"fixture_pool_agent","pool_id":contribution_id,
        "request":{"roomId":dm,"expectedPoolRevision":pool["revision"],"expectedGrantRevision":pool["grantRevision"],
            "definition":{"name":"Offline pooled Agent","capability":pool["capabilities"][0],"description":"Daemon acceptance","toolMode":"none"},"requestedTokens":1000,"renewalMode":"owner"}})).await;
        let request_id = request["request"]["id"]
            .as_str()
            .expect("pool request id")
            .to_owned();
        let approved=central(&manager,json!({"operation":"pool_agent_decide","command_id":"fixture_approve_agent","request_id":request_id,
        "expected_revision":request["request"]["revision"],"approve":true})).await;
        assert_eq!(approved["request"]["state"], "approved_waiting_executor");
        access.retire();
        drop(access);
        manager.retire();
        drop(manager);
        drop(selection);
        native.suspend().await.unwrap();
        drop(native);
        stage("pooled Agent approved; handing original adopted root to daemon");
        let home = root.join("daemon");
        hagency_store::private::directory(&home).unwrap();
        let matrix = home.join(daemon::MATRIX);
        hagency_store::private::directory(&matrix).unwrap();
        let identity = http(
            base,
            daemon_token,
            reqwest::Method::GET,
            "/_matrix/client/v3/account/whoami",
            None,
        )
        .await;
        hagency_store::private::write_new(&matrix.join("session.json"),&serde_json::to_vec(&json!({"version":1,"homeserver":origin,"owner":owner,
        "oauthClientId":fixture["daemon"]["clientId"],"user":{"user_id":owner,"device_id":identity["device_id"],"access_token":daemon_token,
        "refresh_token":fixture["daemon"].get("refreshToken")}})).unwrap()).unwrap();
        Enrollment::enroll(&home, &origin, owner, None, &native_root).unwrap();
        let daemon = DaemonProcess::start(&home, &root.join("daemon.log")).await;
        let a = ControllerClient::new(home.clone(), owner.into(), origin.clone(), 1);
        let b = ControllerClient::new(home.clone(), owner.into(), origin.clone(), 1);
        let remote = a.management(package(5)).await.unwrap();
        let other = b.management(package(6)).await.unwrap();
        assert_eq!(
            other
                .resources(ResourceQuery::default())
                .await
                .unwrap()
                .resources
                .len(),
            1
        );
        let mut ready = json!({});
        for _ in 0..180 {
            ready = remote
                .central(operation(json!({"operation":"pool_agent","id":request_id})))
                .await
                .unwrap();
            if ready["request"]["membershipState"] == "matrix_verified" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let membership = remote
            .central(operation(
                json!({"operation":"agent_membership","id":request_id}),
            ))
            .await
            .unwrap();
        let puppet = membership["membership"]["puppetMxid"]
            .as_str()
            .expect("pooled puppet identity")
            .to_owned();
        assert_eq!(
            ready["request"]["membershipState"], "matrix_verified",
            "{ready} {membership}"
        );
        stage("daemon restored adopted selection and joined pooled Agent");
        let mut dashboard = None;
        for _ in 0..90 {
            let mut client = daemon::DaemonClient::connect(&home).await.unwrap();
            let reply = client
                .call(&daemon::Request::new(
                    daemon::new_request_id(),
                    Some(owner.into()),
                    daemon::Command::Snapshot {
                        period: None,
                        usage_filter: None,
                    },
                ))
                .await
                .unwrap();
            let snapshot = reply.snapshot().unwrap();
            let ready = snapshot.accounts.iter().any(|account| {
                account.provider == "openai"
                    && account.state == "fresh"
                    && account
                        .quota_windows
                        .iter()
                        .any(|window| window.used_percent == Some(25.0))
            });
            dashboard = Some(snapshot);
            if ready {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let dashboard = dashboard.unwrap();
        let account = dashboard
        .accounts
        .iter()
        .find(|account| account.provider == "openai" && account.state == "fresh")
        .expect(
            "adopted original provider account has an observed quota report before any model turn",
        );
        assert!(
            account
                .quota_windows
                .iter()
                .any(|window| window.used_percent == Some(25.0)
                    && window.window_minutes == Some(300))
        );
        assert!(account.quota_windows.iter().any(
            |window| window.used_percent == Some(40.0) && window.window_minutes == Some(10080)
        ));
        stage(
            "adopted daemon reports original Codex account short and weekly windows before model use",
        );

        // A separate owner-authorized room retains its own durable binding and workspace.
        let agent_id = ready["request"]["agentId"]
            .as_str()
            .expect("pooled Agent id")
            .to_owned();
        let space = http(base, human_token, reqwest::Method::POST, "/_matrix/client/v3/createRoom",
            Some(json!({"preset":"private_chat","name":"Pooled encrypted space","creation_content":{"type":"m.space"}}))).await;
        let group = http(base, human_token, reqwest::Method::POST, "/_matrix/client/v3/createRoom",
            Some(json!({"preset":"private_chat","name":"Separate pooled encrypted group","invite":[format!("@_hagency_service:{}",owner.split_once(':').unwrap().1)],"initial_state":[{"type":"m.room.encryption","state_key":"","content":{"algorithm":"m.megolm.v1.aes-sha2"}}]}))).await;
        let group = group["room_id"].as_str().unwrap().to_owned();
        http(
            base,
            human_token,
            reqwest::Method::PUT,
            &format!(
                "/_matrix/client/v3/rooms/{}/state/m.space.child/{}",
                segment(space["room_id"].as_str().unwrap()),
                segment(&group)
            ),
            Some(json!({"via":[owner.split_once(':').unwrap().1]})),
        )
        .await;
        if native_admin {
            let projects = a.execute(Command::Projects).await.unwrap();
            assert!(projects["projects"].is_array());
            let candidates = a
                .execute(Command::SpaceCandidates { cursor: None })
                .await
                .unwrap();
            assert!(candidates["spaces"].is_array(), "{candidates}");
        }
        let project = if native_admin {
            a.execute(Command::AdoptProject {
                space_id: space["room_id"].as_str().unwrap().into(),
            })
            .await
            .unwrap()
        } else {
            owner_api(
                base,
                human_token,
                reqwest::Method::POST,
                "/hagency/api/v1/projects/adopt",
                Some(json!({"spaceId":space["room_id"]})),
            )
            .await
        };
        let project_id = project["project"]["id"].as_str().unwrap();
        if native_admin {
            let rooms = a
                .execute(Command::ProjectRooms {
                    project_id: project_id.into(),
                })
                .await
                .unwrap();
            assert!(rooms["rooms"].is_array());
            a.execute(Command::AdoptRoom {
                project_id: project_id.into(),
                room_id: group.clone(),
            })
            .await
            .unwrap();
            let rooms = a
                .execute(Command::ProjectRooms {
                    project_id: project_id.into(),
                })
                .await
                .unwrap();
            assert!(
                rooms["rooms"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|room| room["roomId"] == group)
            );
            let agents = a.execute(Command::RoomAgents).await.unwrap();
            assert!(
                agents["agents"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|agent| agent["id"] == agent_id),
                "pooled Agent visible in actual native picker: {agents}"
            );
        } else {
            owner_api(
                base,
                human_token,
                reqwest::Method::POST,
                &format!("/hagency/api/v1/projects/{project_id}/rooms/adopt"),
                Some(json!({"roomId":group})),
            )
            .await;
        }
        let binding_path = format!("/hagency/api/v1/agents/{agent_id}/bindings");
        let binding_key = unique_id("pooled_group_bind");
        let input = json!({"projectId":project_id,"roomId":group,"idempotencyKey":binding_key});
        let bound = if native_admin {
            a.execute(Command::BindAgent {
                agent_id: agent_id.clone(),
                input: input.clone(),
            })
            .await
            .unwrap()
        } else {
            owner_api(
                base,
                human_token,
                reqwest::Method::POST,
                &binding_path,
                Some(input.clone()),
            )
            .await
        };
        let command_path = format!("/hagency/api/v1/commands/agent.bind/{binding_key}");
        let group_binding = bound["creation"]["binding"]["id"]
            .as_str()
            .expect("additional room binding")
            .to_owned();
        let mut binding_active = false;
        for _ in 0..90 {
            let bindings =
                owner_api(base, human_token, reqwest::Method::GET, &command_path, None).await;
            if (bindings["creation"]["binding"]["id"] == group_binding
                && bindings["creation"]["binding"]["state"] == "active")
            {
                binding_active = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        assert!(
            binding_active,
            "additional pooled room binding must become active"
        );
        let joined = http(
            base,
            human_token,
            reqwest::Method::GET,
            &format!(
                "/_matrix/client/v3/rooms/{}/joined_members",
                segment(&group)
            ),
            None,
        )
        .await;
        assert!(
            joined["joined"].get(&puppet).is_some(),
            "pooled Agent must actually join additional room"
        );
        http(
            base,
            human_token,
            reqwest::Method::POST,
            &format!("/_matrix/client/v3/rooms/{}/invite", segment(&group)),
            Some(json!({"user_id":member_owner})),
        )
        .await;
        http(
            base,
            member_token,
            reqwest::Method::POST,
            &format!("/_matrix/client/v3/rooms/{}/join", segment(&group)),
            Some(json!({})),
        )
        .await;
        if native_admin {
            let commands = a.execute(Command::AgentCommands).await.unwrap();
            let rows = commands["commands"]
                .as_array()
                .expect("original command journal");
            for command in rows
                .iter()
                .filter(|command| command["input"]["idempotencyKey"] == binding_key)
            {
                let resumed = a
                    .execute(Command::ResumeAgentCommand {
                        id: command["id"].as_str().unwrap().into(),
                    })
                    .await
                    .unwrap();
                assert_eq!(
                    resumed["creation"]["binding"]["id"], group_binding,
                    "saved resume keeps original binding"
                );
            }
            let repeated = a
                .execute(Command::BindAgent {
                    agent_id: agent_id.clone(),
                    input,
                })
                .await
                .unwrap();
            assert_eq!(
                repeated["creation"]["binding"]["id"], group_binding,
                "same native command resolves its original receipt"
            );
            let roster = a
                .execute(Command::RoomRoster {
                    project_id: project_id.into(),
                    room_id: group.clone(),
                })
                .await
                .unwrap();
            assert!(
                roster["agents"].as_array().unwrap().iter().any(|agent| {
                    agent["agentId"] == agent_id
                        && agent["bindingState"] == "active"
                        && agent["ownerMxid"] == owner
                }),
                "actual room roster exposes active shared Agent and owner: {roster}"
            );
            stage(
                "native adopted Projects/adoption/room picker/BindAgent/original command journal/RoomRoster succeeded",
            );
        }
        stage("separate encrypted group bound with real requester and second human");

        remote.retire();
        other.retire();
        drop(remote);
        drop(other);
        a.retire();
        b.retire();
        drop(a);
        drop(b);
        if native_admin {
            assert!(
                turns(&marker).is_empty(),
                "server administration cannot start model execution"
            );
            daemon.stop().await;
            println!(
                "PASS: adopted daemon original Native room administration, shared Agent picker/binding, original receipt replay and roster; no legacy execution"
            );
            hagency_store::private::write_new(
                &fixture_path.parent().unwrap().join("client-done.json"),
                b"{\"passed\":true}",
            )
            .unwrap();
            return;
        }
        let human_identity = http(
            base,
            human_token,
            reqwest::Method::GET,
            "/_matrix/client/v3/account/whoami",
            None,
        )
        .await;
        let human = Client::builder()
            .homeserver_url(base)
            .sqlite_store(root.join("human-sdk"), Some("isolated-fixture-store-key"))
            .build()
            .await
            .unwrap();
        human
            .matrix_auth()
            .restore_session(
                MatrixSession {
                    meta: SessionMeta {
                        user_id: owner.try_into().unwrap(),
                        device_id: human_identity["device_id"].as_str().unwrap().into(),
                    },
                    tokens: SessionTokens {
                        access_token: human_token.into(),
                        refresh_token: None,
                    },
                },
                RoomLoadSettings::default(),
            )
            .await
            .unwrap();
        let replies = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
        let output = replies.clone();
        let relations = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
        let related = relations.clone();
        let expected_sender = puppet.to_owned();
        human.add_event_handler(
            move |event: OriginalSyncRoomMessageEvent, room: matrix_sdk::Room| {
                let output = output.clone();
                let related = related.clone();
                let expected_sender = expected_sender.clone();
                async move {
                    if event.sender.as_str() == expected_sender
                        && event.content.body().contains(ANSWER)
                    {
                        related
                            .lock()
                            .unwrap()
                            .push((room.room_id().to_string(), reply_parent(&event.content)));
                        output
                            .lock()
                            .unwrap()
                            .push((room.room_id().to_string(), event.event_id.to_string()));
                    }
                }
            },
        );
        human
            .sync_once(SyncSettings::default().timeout(Duration::ZERO))
            .await
            .unwrap();
        let sync_client = human.clone();
        let sync = tokio::spawn(async move {
            sync_client
                .sync(SyncSettings::default().timeout(Duration::from_secs(1)))
                .await
        });
        let member_identity = http(
            base,
            member_token,
            reqwest::Method::GET,
            "/_matrix/client/v3/account/whoami",
            None,
        )
        .await;
        let member = Client::builder()
            .homeserver_url(base)
            .sqlite_store(root.join("member-sdk"), Some("isolated-member-key"))
            .build()
            .await
            .unwrap();
        member
            .matrix_auth()
            .restore_session(
                MatrixSession {
                    meta: SessionMeta {
                        user_id: member_owner.try_into().unwrap(),
                        device_id: member_identity["device_id"].as_str().unwrap().into(),
                    },
                    tokens: SessionTokens {
                        access_token: member_token.into(),
                        refresh_token: None,
                    },
                },
                RoomLoadSettings::default(),
            )
            .await
            .unwrap();
        let member_replies = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
        let member_output = member_replies.clone();
        let member_sender = puppet.to_owned();
        member.add_event_handler(
            move |event: OriginalSyncRoomMessageEvent, room: matrix_sdk::Room| {
                let output = member_output.clone();
                let sender = member_sender.clone();
                async move {
                    if event.sender.as_str() == sender && event.content.body().contains(ANSWER) {
                        output
                            .lock()
                            .unwrap()
                            .push((room.room_id().to_string(), reply_parent(&event.content)));
                    }
                }
            },
        );
        member
            .sync_once(SyncSettings::default().timeout(Duration::ZERO))
            .await
            .unwrap();
        let member_sync_client = member.clone();
        let member_sync = tokio::spawn(async move {
            member_sync_client
                .sync(SyncSettings::default().timeout(Duration::from_secs(1)))
                .await
        });
        // Wait for the actual Agent device key upload before sharing a room key.
        let mut keys = json!({});
        for _ in 0..120 {
            keys = http(
                base,
                human_token,
                reqwest::Method::POST,
                "/_matrix/client/v3/keys/query",
                Some(json!({"device_keys":{puppet.as_str():[]}})),
            )
            .await;
            if keys["device_keys"][puppet.as_str()]
                .as_object()
                .is_some_and(|v| !v.is_empty())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        assert!(
            !keys["device_keys"][puppet.as_str()]
                .as_object()
                .unwrap()
                .is_empty(),
            "Agent encryption keys were not uploaded"
        );
        let original_keys = keys["device_keys"][puppet.as_str()].clone();
        let mut expected_replies = Vec::new();

        let event = send_expected(
            &human,
            dm,
            "POOLED_ENCRYPTED_AFTER_BOTH_CONTROLLERS_DETACH",
            None,
            &mut expected_replies,
        )
        .await;
        let wire = http(
            base,
            human_token,
            reqwest::Method::GET,
            &format!(
                "/_matrix/client/v3/rooms/{}/event/{}",
                segment(dm),
                segment(&event)
            ),
            None,
        )
        .await;
        assert_eq!(wire["type"], "m.room.encrypted");
        assert!(wire["content"]["body"].is_null());
        reply_count(&replies, &marker, 1).await;
        stage(
            "pooled encrypted request executed and reply decrypted after both controllers detached",
        );
        // The requester alone can talk in the original room without @. A real
        // second human changes that policy for BOTH senders immediately.
        http(
            base,
            human_token,
            reqwest::Method::POST,
            &format!("/_matrix/client/v3/rooms/{}/invite", segment(dm)),
            Some(json!({"user_id":member_owner})),
        )
        .await;
        http(
            base,
            member_token,
            reqwest::Method::POST,
            &format!("/_matrix/client/v3/rooms/{}/join", segment(dm)),
            Some(json!({})),
        )
        .await;
        let dm_id: OwnedRoomId = dm.try_into().unwrap();
        for _ in 0..90 {
            if member
                .get_room(&dm_id)
                .is_some_and(|room| room.state() == matrix_sdk::RoomState::Joined)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let joined = http(
            base,
            human_token,
            reqwest::Method::GET,
            &format!("/_matrix/client/v3/rooms/{}/joined_members", segment(dm)),
            None,
        )
        .await;
        assert!(
            joined["joined"].get(owner).is_some()
                && joined["joined"].get(member_owner).is_some()
                && joined["joined"].get(&puppet).is_some()
        );
        send(&human, dm, "OWNER_NO_MENTION_AFTER_SECOND_HUMAN", None).await;
        send(&member, dm, "MEMBER_NO_MENTION_AFTER_JOIN", None).await;
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert_eq!(
            turns(&marker).len(),
            1,
            "neither human may trigger group execution without explicit @"
        );
        send_expected(
            &human,
            dm,
            "OWNER_EXPLICIT_MENTION",
            Some(&puppet),
            &mut expected_replies,
        )
        .await;
        reply_count(&replies, &marker, 2).await;
        send_expected(
            &member,
            dm,
            "MEMBER_EXPLICIT_MENTION",
            Some(&puppet),
            &mut expected_replies,
        )
        .await;
        reply_count(&replies, &marker, 3).await;
        send(&human, &group, "SEPARATE_GROUP_OWNER_WITHOUT_MENTION", None).await;
        send(
            &member,
            &group,
            "SEPARATE_GROUP_MEMBER_WITHOUT_MENTION",
            None,
        )
        .await;
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert_eq!(
            turns(&marker).len(),
            3,
            "bound group also requires @ from both humans"
        );
        send_expected(
            &member,
            &group,
            "SEPARATE_GROUP_MEMBER_MENTION",
            Some(&puppet),
            &mut expected_replies,
        )
        .await;
        reply_count(&replies, &marker, 4).await;
        stage(
            "both humans require explicit @ after DM becomes a group; separate bound group executes encrypted member mention",
        );
        daemon.stop().await;
        let daemon = DaemonProcess::start(&home, &root.join("daemon.log")).await;
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert_eq!(
            turns(&marker).len(),
            4,
            "restart must not duplicate accepted pooled turns"
        );
        send_expected(
            &human,
            dm,
            "POOLED_ENCRYPTED_AFTER_RESTART",
            Some(&puppet),
            &mut expected_replies,
        )
        .await;
        reply_count(&replies, &marker, 5).await;
        send_expected(
            &member,
            &group,
            "GROUP_ENCRYPTED_AFTER_RESTART",
            Some(&puppet),
            &mut expected_replies,
        )
        .await;
        reply_count(&replies, &marker, 6).await;
        let recorded = turns(&marker);
        for index in [1, 2, 4] {
            assert_eq!(
                recorded[0]["cwd"], recorded[index]["cwd"],
                "same pooled room preserves directory"
            );
        }
        assert_eq!(
            recorded[3]["cwd"], recorded[5]["cwd"],
            "separate room survives restart"
        );
        assert_ne!(
            recorded[0]["cwd"], recorded[3]["cwd"],
            "distinct pooled rooms cannot share working directories"
        );
        for item in &recorded {
            assert!(Path::new(item["cwd"].as_str().unwrap()).starts_with(&native_root));
        }

        let mut observed = relations.lock().unwrap().clone();
        observed.sort();
        let mut expected = expected_replies.clone();
        expected.sort();
        assert_eq!(
            observed, expected,
            "every decrypted owner-visible reply must target the exact accepted request, never a suppressed message"
        );
        let member_expected: Vec<_> = expected_replies.iter().skip(1).cloned().collect();
        for _ in 0..50 {
            if member_expected
                .iter()
                .all(|item| member_replies.lock().unwrap().contains(item))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            member_expected
                .iter()
                .all(|item| member_replies.lock().unwrap().contains(item)),
            "second human must decrypt replies to their actual group requests"
        );
        let expected_inputs = [
            "POOLED_ENCRYPTED_AFTER_BOTH_CONTROLLERS_DETACH",
            "OWNER_EXPLICIT_MENTION",
            "MEMBER_EXPLICIT_MENTION",
            "SEPARATE_GROUP_MEMBER_MENTION",
            "POOLED_ENCRYPTED_AFTER_RESTART",
            "GROUP_ENCRYPTED_AFTER_RESTART",
        ];
        for (recorded, expected) in turns(&marker).iter().zip(expected_inputs) {
            let inputs = recorded["input"].as_array().expect("provider inputs");
            assert!(
                inputs.iter().any(|input| input["text"] == expected),
                "exact accepted text must reach provider: {recorded}"
            );
        }
        let current = http(
            base,
            human_token,
            reqwest::Method::POST,
            "/_matrix/client/v3/keys/query",
            Some(json!({"device_keys":{puppet.as_str():[]}})),
        )
        .await;
        assert_eq!(
            current["device_keys"][puppet.as_str()],
            original_keys,
            "pooled crypto identity survives restart"
        );
        for (room, event) in replies.lock().unwrap().clone() {
            let wire = http(
                base,
                human_token,
                reqwest::Method::GET,
                &format!(
                    "/_matrix/client/v3/rooms/{}/event/{}",
                    segment(&room),
                    segment(&event)
                ),
                None,
            )
            .await;
            assert_eq!(wire["type"], "m.room.encrypted");
            assert!(wire["content"]["body"].is_null());
        }
        // A new observer after the six model turns inspects real server/native usage;
        // it does not supply execution authority for either request.
        let observer = ControllerClient::new(home.clone(), owner.into(), origin.clone(), 1);
        let remote = observer.management(package(7)).await.unwrap();
        let mut budget = json!({});
        for _ in 0..60 {
            budget = remote
                .central(operation(
                    json!({"operation":"agent_budget","id":request_id}),
                ))
                .await
                .unwrap();
            if budget["budget"]["knownConsumedTokens"] == 30 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        assert_eq!(budget["budget"]["knownConsumedTokens"], 30, "{budget}");
        assert_eq!(budget["budget"]["effectiveCeilingTokens"], 1000);
        // A live Agent retains its declared capacity for later turns. Per-call
        // liabilities below must settle to actual usage; its total ceiling stays reserved.
        assert_eq!(budget["budget"]["conservativeCommitmentTokens"], 1000);
        let work = remote
            .central(operation(
                json!({"operation":"agent_work","id":request_id,"limit":16}),
            ))
            .await
            .unwrap();
        let items = work["work"]["items"].as_array().expect("settled work rows");
        assert_eq!(
            items.len(),
            6,
            "restart must not add a duplicate accepted work: {work}"
        );
        for item in items {
            assert_eq!(item["state"], "completed", "{item}");
            assert_eq!(item["usage"]["knownConsumedTokens"], 5, "{item}");
            assert_eq!(item["usage"]["retainedTokens"], 5, "{item}");
            assert_eq!(item["usage"]["complete"], true, "{item}");
            assert!(item["completedAtMs"].as_i64().is_some_and(|at| at > 0));
        }
        stage(
            "real server recorded six completed work receipts, 30 consumed tokens, no uncertain liability",
        );
        let mut client = daemon::DaemonClient::connect(&home).await.unwrap();
        let snapshot = client
            .call(&daemon::Request::new(
                daemon::new_request_id(),
                Some(owner.into()),
                daemon::Command::Snapshot {
                    period: None,
                    usage_filter: None,
                },
            ))
            .await
            .unwrap()
            .snapshot()
            .unwrap();
        assert_eq!(
            snapshot.usage.counts.input,
            Some(18),
            "dashboard retains pre-restart input usage"
        );
        assert_eq!(snapshot.usage.counts.output, Some(12));
        assert_eq!(snapshot.usage.counts.display_volume().unwrap(), Some(30));
        // This collector began after the selected calendar day started. The
        // observed turns are complete; earlier unobserved history stays unknown.
        let total = &snapshot
            .usage_breakdown
            .as_ref()
            .expect("usage breakdown")
            .report
            .total;
        assert!(!snapshot.usage.complete && total.coverage_gap);
        assert_eq!(total.executions, 6);
        assert_eq!(total.unmeasured_executions, 0);
        assert_eq!(total.diagnostics.conflicts, 0);
        assert_eq!(total.diagnostics.regressions, 0);
        assert_eq!(total.diagnostics.undeduplicable, 0);
        assert_eq!(total.diagnostics.incomplete_captures, 0);
        assert_eq!(total.diagnostics.missing_after_known, 0);
        stage("daemon dashboard persisted exact six-turn usage across restart");
        let bindings =
            owner_api(base, human_token, reqwest::Method::GET, &command_path, None).await;
        assert!(
            (bindings["creation"]["binding"]["id"] == group_binding
                && bindings["creation"]["binding"]["state"] == "active"),
            "same original group binding survives restart"
        );
        http(
            base,
            human_token,
            reqwest::Method::POST,
            &format!("/_matrix/client/v3/rooms/{}/leave", segment(&group)),
            Some(json!({})),
        )
        .await;
        let mut fenced = false;
        for _ in 0..90 {
            let bindings =
                owner_api(base, human_token, reqwest::Method::GET, &command_path, None).await;
            if (bindings["creation"]["binding"]["id"] == group_binding
                && bindings["creation"]["binding"]["state"] == "left")
            {
                fenced = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        assert!(
            fenced,
            "owner departure must permanently fence extra room binding"
        );
        send(
            &member,
            &group,
            "OWNER_ABSENT_EXPLICIT_MENTION_MUST_NOT_RUN",
            Some(&puppet),
        )
        .await;
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert_eq!(
            turns(&marker).len(),
            6,
            "owner absence revokes room execution even with explicit @"
        );
        stage(
            "owner departure fenced original extra-room binding; remaining member mention caused no execution",
        );
        remote.retire();
        drop(remote);
        observer.retire();
        drop(observer);
        daemon.stop().await;
        sync.abort();
        member_sync.abort();
        let _ = sync.await;
        let _ = member_sync.await;
        println!(
            "PASS: real pooled daemon encrypted execution after two controller detaches and process restart, six deterministic turns, two real humans, encrypted group mention rules, distinct room directories and owner membership fencing"
        );
        hagency_store::private::write_new(
            &fixture_path.parent().unwrap().join("client-done.json"),
            b"{\"passed\":true}",
        )
        .unwrap();
    }

    async fn isolated_pooled_daemon_process() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .try_init();
        let root = PathBuf::from(std::env::var_os("HAGENCY_DAEMON_CRYPTO_ROOT").unwrap())
            .canonicalize()
            .unwrap();
        initialize_resource_activation_fixture(&root).unwrap();
        let home = PathBuf::from(std::env::var_os("HAGENCY_DAEMON_CRYPTO_HOME").unwrap());
        let cancel = hagency_matrix::CancellationToken::new();
        let signal = cancel.clone();
        tokio::spawn(async move {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
            term.recv().await;
            signal.cancel();
        });
        host::run(&home, cancel).await.unwrap();
    }

    pub(super) fn run() {
        let args: Vec<_> = std::env::args().skip(1).collect();
        if args.first().is_some_and(|a| a == "guardian") {
            hagency_platform::run_guardian().unwrap();
            return;
        }
        if !args.iter().any(|a| a == "--ignored") {
            println!(
                "SKIPPED: isolated pooled daemon acceptance requires --ignored and a private loopback fixture"
            );
            return;
        }
        if args.iter().any(|a| a == "isolated_pooled_daemon_process") {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .enable_all()
                .build()
                .unwrap()
                .block_on(isolated_pooled_daemon_process());
        } else if args
            .iter()
            .any(|a| a == "real_pooled_daemon_encrypted_child")
        {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .enable_all()
                .build()
                .unwrap()
                .block_on(real_pooled_daemon_encrypted_child());
        } else {
            real_pooled_daemon_encrypted_restart();
        }
    }
}

#[cfg(unix)]
fn main() {
    fixture::run();
}

#[cfg(not(unix))]
fn main() {
    println!("SKIPPED: owner daemon socket acceptance is Unix-only");
}
