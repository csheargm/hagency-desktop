use super::*;
use crate::file_service::test_common as common;
use serde_json::json;

fn input() -> Existing {
    let resource = common::domain::resource("pool", "seat", 1000);
    let request = common::domain::request("adoption", "Worker", &resource, 100);
    serde_json::from_value(json!({"origin":"https://example.test/","observer_mxid":"@owner:example.test",
        "observer_device_id":"OWNER_DEVICE","agent_mxid":"@worker:example.test","agent_device_id":"DEVICE_1",
        "agent_room_id":"!direct:example.test","session_id":"direct_session","workspace_id":"direct_work",
        "registration":common::domain::registration(),"resource":resource,"request":request,
        "project_inbox":{"session_id":"group_session","workspace_id":"group_work"}})).unwrap()
}
fn states(room: &RoomObservation) -> Value {
    let mut events: Vec<Value> = room
        .joined
        .iter()
        .map(|id| json!({"type":"m.room.member","state_key":id,"content":{"membership":"join"}}))
        .collect();
    events.push(json!({"type":"m.room.join_rules","state_key":"","content":{"join_rule":if room.invite_only {"invite"} else {"public"}}}));
    events.push(json!({"type":"m.room.power_levels","state_key":"","content":{"users":room.powers,"users_default":room.default_power,"invite":room.invite_power}}));
    if let Some(encryption) = &room.encryption {
        events.push(
            json!({"type":"m.room.encryption","state_key":"","content":{"algorithm":encryption}}),
        );
    }
    if let Some(binding) = &room.binding {
        events.push(
            json!({"type":"com.hagency.admin.binding.v1","state_key":binding.get("fleetId").cloned().unwrap_or_default(),"content":binding}),
        );
    }
    if let Some(name) = &room.name {
        events.push(json!({"type":"m.room.name","state_key":"","content":{"name":name}}));
    }
    json!(events)
}
fn client(origin: &str, token: &str) -> Matrix {
    let mut matrix = Matrix::new(origin, token.as_bytes().to_vec()).unwrap();
    matrix.client = reqwest::Client::builder()
        .redirect(Policy::none())
        .no_proxy()
        .add_root_certificate(
            reqwest::Certificate::from_pem(include_bytes!(
                "../../../../hagency-matrix/tests/fixtures/ca.pem"
            ))
            .unwrap(),
        )
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    matrix
}
/// Board #95: which binding event(s) the request's target project room carries.
#[derive(Clone, Default)]
struct Bindings {
    /// Drop the TS `com.hagency.admin.binding.v1` event.
    without_ts: bool,
    /// Also carry the LEGACY `com.hagency.project.binding.v1` event (key `""`)
    /// naming this fleet — the shape an earlier Rust build wrote.
    legacy_fleet: Option<String>,
}

/// Board #95: the project-room state with the binding event(s) a test wants.
fn project_state(project: &RoomObservation, bindings: &Bindings) -> Value {
    let mut state = states(project);
    let mut binding = None;
    state.as_array_mut().unwrap().retain(|event| {
        if event["type"] == "com.hagency.admin.binding.v1" {
            binding = Some(event["content"].clone());
            !bindings.without_ts
        } else {
            true
        }
    });
    if let Some(fleet) = &bindings.legacy_fleet {
        let mut legacy = binding
            .or_else(|| project.binding.clone())
            .expect("the fixture's project room carries a binding");
        legacy["fleetId"] = json!(fleet);
        state.as_array_mut().unwrap().push(json!({
            "type": "com.hagency.project.binding.v1",
            "state_key": "",
            "content": legacy
        }));
    }
    state
}

async fn exercise(state: &Path, input: Existing, bad: Option<&str>) -> Result<Receipt, Error> {
    exercise_bindings(state, input, bad, Bindings::default()).await
}

async fn exercise_bindings(
    state: &Path,
    mut input: Existing,
    bad: Option<&str>,
    bindings: Bindings,
) -> Result<Receipt, Error> {
    let mut fake = common::Fake::start(true).await;
    input.origin = fake.endpoint.clone();
    let mut observation = common::domain::observation(&input.request);
    observation.project.joined.insert(input.agent_mxid.clone());
    let project = project_state(&observation.project, &bindings);
    let mut agent_project = project_state(&observation.project, &bindings);
    if let Some(missing) = bad {
        agent_project
            .as_array_mut()
            .unwrap()
            .retain(|event| event["state_key"] != missing);
    }
    let source = &observation.source;
    let mut responses = vec![
        (
            "observer",
            json!({"user_id":input.observer_mxid,"device_id":input.observer_device_id}),
        ),
        (
            "agent",
            json!({"user_id":input.agent_mxid,"device_id":input.agent_device_id}),
        ),
        (
            "observer",
            json!({"event_id":source.event_id,"room_id":source.room_id,"sender":source.sender,"type":source.event_type,"content":source.content}),
        ),
        ("observer", states(&observation.reception)),
        ("observer", project),
        ("observer", states(&observation.owner_room)),
        ("agent", common::state()),
    ];
    if input.project_inbox.is_some() {
        responses.push(("agent", agent_project));
    }
    let observer = client(&fake.endpoint, "synthetic-observer-token");
    let agent = client(&fake.endpoint, "synthetic-agent-token");
    let scripted = async {
        for (i, (owner, value)) in responses.into_iter().enumerate() {
            let request = fake.next().await;
            assert_eq!(request.method, "GET");
            assert_eq!(
                request.headers["authorization"],
                format!("Bearer synthetic-{owner}-token")
            );
            if i < 2 {
                assert!(request.target.ends_with("/account/whoami"));
            }
            if i == 6 {
                assert!(request.target.contains("direct") && request.target.ends_with("/state"));
            }
            if i == 7 {
                assert!(request.target.contains("project") && request.target.ends_with("/state"));
            }
            request.json(200, value);
        }
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(adopt(state, input, observer, agent), scripted)
    })
    .await
    .unwrap();
    fake.close().await;
    result
}

#[tokio::test]
async fn native_adopt_project_inbox() {
    let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let state = root.path().join("state");
    private::directory(&state).unwrap();
    for _ in 0..2 {
        let receipt = exercise(&state, input(), None).await.unwrap();
        assert_eq!(receipt.session_id, "direct_session");
        assert_eq!(receipt.project_inbox.unwrap().session_id, "group_session");
        let sql = rusqlite::Connection::open(state.join("domain.sqlite3")).unwrap();
        assert_eq!(
            sql.query_row("SELECT COUNT(*) FROM runner_sessions", [], |r| r
                .get::<_, u64>(0))
                .unwrap(),
            2
        );
        for (session, room, kind, encrypted) in [
            ("direct_session", "!direct:example.test", "direct", true),
            ("group_session", "!project:example.test", "group", false),
        ] {
            let route:String=sql.query_row("SELECT r.config FROM matrix_session_routes r JOIN current_matrix_routes c ON c.session_id=r.session_id WHERE r.session_id=?1",[session],|r|r.get(0)).unwrap();
            let route: Value = serde_json::from_str(&route).unwrap();
            assert_eq!(route["room_id"], room);
            assert_eq!(route["privacy"]["kind"], kind);
            assert_eq!(route["encrypted"], encrypted);
        }
        assert_eq!(
            sql.query_row("SELECT COUNT(*) FROM workspace_resources", [], |r| r
                .get::<_, u64>(0))
                .unwrap(),
            2
        );
    }
    let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let state = root.path().join("state");
    private::directory(&state).unwrap();
    let mut document = input();
    document.project_inbox = None;
    let receipt = exercise(&state, document, None).await.unwrap();
    assert!(
        serde_json::to_value(receipt)
            .unwrap()
            .get("project_inbox")
            .is_none()
    );
}

#[tokio::test]
async fn native_adopt_project_inbox_refusals() {
    for missing in ["@worker:example.test", "@owner:example.test"] {
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let state = root.path().join("state");
        private::directory(&state).unwrap();
        assert!(matches!(
            exercise(&state, input(), Some(missing)).await,
            Err(Error::Authority)
        ));
        assert!(!state.join("domain.sqlite3").exists());
    }
    for case in ["session", "workspace", "generation", "path", "room"] {
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let state = root.path().join("state");
        private::directory(&state).unwrap();
        let mut fake = common::Fake::start(true).await;
        let mut document = input();
        let inbox = document.project_inbox.as_mut().unwrap();
        match case {
            "session" => inbox.session_id = document.session_id.clone(),
            "workspace" => inbox.workspace_id = document.workspace_id.clone(),
            "generation" => inbox.room_generation = 0,
            "path" => inbox.workspace_id = "../private".into(),
            _ => document.agent_room_id = document.request.target_room_id.clone(),
        }
        assert!(matches!(
            adopt(
                &state,
                document,
                client(&fake.endpoint, "synthetic-observer-token"),
                client(&fake.endpoint, "synthetic-agent-token")
            )
            .await,
            Err(Error::Document)
        ));
        assert!(!state.join("domain.sqlite3").exists());
        assert_eq!(fake.requests(), 0);
        fake.no_request().await;
        fake.close().await;
    }
    let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let state = root.path().join("state");
    private::directory(&state).unwrap();
    exercise(&state, input(), None).await.unwrap();
    for direct in [false, true] {
        let mut document = input();
        if direct {
            document.session_id = "new_direct_name".into();
        } else {
            document.project_inbox.as_mut().unwrap().session_id = "new_group_name".into();
        }
        assert!(
            matches!(exercise(&state,document,None).await,Err(Error::StateAt(stage)) if stage==if direct {"session_identity"} else {"project_session_identity"})
        );
        let sql = rusqlite::Connection::open(state.join("domain.sqlite3")).unwrap();
        assert_eq!(
            sql.query_row("SELECT COUNT(*) FROM runner_sessions", [], |r| r
                .get::<_, u64>(0))
                .unwrap(),
            2
        );
    }
}

#[tokio::test]
async fn native_adopt_matrix_chunked_bound() {
    let mut fake = common::Fake::start(true).await;
    let matrix = client(&fake.endpoint, "synthetic-agent-token");
    let (release, held) = tokio::sync::oneshot::channel();
    let script = async {
        let request = fake.next().await;
        let length = RESPONSE_LIMIT as usize + 1;
        let mut wire=format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n{length:x}\r\n").into_bytes();
        wire.extend(vec![b' '; length]);
        wire.extend_from_slice(b"\r\n");
        request.hold(
            vec![
                (Duration::ZERO, wire),
                (Duration::ZERO, b"0\r\n\r\n".to_vec()),
            ],
            1,
            held,
        );
    };
    // The server withholds the end of the body. Refusal must precede EOF and
    // the HTTP client's five-second deadline, not buffer the entire response.
    let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(matrix.get(&["bounded"]), script)
    })
    .await
    .unwrap();
    assert!(matches!(result, Err(Error::Matrix)));
    drop(release);
    fake.close().await;
}

/// An obsolete unkeyed project binding cannot authorize adoption.
#[tokio::test]
async fn native_adopt_refuses_room_bound_only_by_the_obsolete_event() {
    let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let state = root.path().join("state");
    private::directory(&state).unwrap();
    let bindings = Bindings { without_ts: true, legacy_fleet: Some(common::domain::registration().fleet_id) };
    assert!(matches!(exercise_bindings(&state, input(), None, bindings).await, Err(Error::Authority)));
    assert!(!state.join("domain.sqlite3").exists());
}

/// Board #95: when a room carries BOTH binding events and they disagree, the
/// TS event wins. The legacy event here names a foreign fleet; were the legacy
/// event consulted the adopt would be refused, so success proves the TS event
/// was the one carried.
#[tokio::test]
async fn native_adopt_prefers_the_ts_binding_when_both_are_present() {
    let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let state = root.path().join("state");
    private::directory(&state).unwrap();
    let fleet = common::domain::registration().fleet_id;
    let foreign = format!("hf_{}", "b".repeat(32));
    assert_ne!(foreign, fleet);
    let bindings = Bindings {
        without_ts: false,
        legacy_fleet: Some(foreign),
    };
    let receipt = exercise_bindings(&state, input(), None, bindings)
        .await
        .unwrap();
    assert_eq!(receipt.session_id, "direct_session");
}

/// Board #95: a LEGACY binding naming a foreign fleet is refused with the same
/// error as today — the fall-back reads the event, it does not relax the
/// `verify_request` fleet/project gate.
#[tokio::test]
async fn native_adopt_refuses_a_foreign_fleet_legacy_binding() {
    let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let state = root.path().join("state");
    private::directory(&state).unwrap();
    let foreign = format!("hf_{}", "b".repeat(32));
    assert_ne!(foreign, common::domain::registration().fleet_id);
    let bindings = Bindings {
        without_ts: true,
        legacy_fleet: Some(foreign),
    };
    assert!(matches!(
        exercise_bindings(&state, input(), None, bindings).await,
        Err(Error::Authority)
    ));
    assert!(!state.join("domain.sqlite3").exists());
}
