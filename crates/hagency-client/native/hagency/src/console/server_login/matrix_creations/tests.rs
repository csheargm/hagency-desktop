use super::*;
pub(super) fn input(kind: Kind) -> Input {
    Input {
        command_id: "command-1".into(),
        kind: kind.clone(),
        name: "Discussion".into(),
        project_id: (kind == Kind::Room).then(|| "prj_1".into()),
        space_id: (kind == Kind::Room).then(|| "!space:example.test".into()),
    }
}
pub(super) fn command() -> Command {
    Command {
        input: input(Kind::Room),
        origin: "https://example.test/".into(),
        owner: "@owner:example.test".into(),
        subject: "pasion-subject".into(),
        room_id: Some("!room:example.test".into()),
        phase: "created".into(),
        project: None,
        last_error: None,
    }
}
pub(super) fn state(c: &Command) -> Value {
    json!([
     {"type":"m.room.create","state_key":"","sender":c.owner,"content":{"creator":c.owner}},
     {"type":MARKER,"state_key":"","sender":c.owner,"content":{"commandId":c.input.command_id,"owner":c.owner,"kind":c.input.kind,"projectId":c.input.project_id,"spaceId":c.input.space_id}},
     {"type":"m.room.join_rules","state_key":"","sender":c.owner,"content":{"join_rule":"invite"}}
    ])
}
#[test]
fn private_creation_and_closed_operations() {
    let (_, path, body) = MatrixOperation::Create(
        input(Kind::Room),
        "@owner:example.test".into(),
        "@_hagency_service:example.test".into(),
    )
    .request()
    .unwrap();
    assert_eq!(path, "/_matrix/client/v3/createRoom");
    let body = body.unwrap();
    assert_eq!(body["visibility"], "private");
    assert_eq!(body["preset"], "private_chat");
    assert_eq!(body["invite"], json!(["@_hagency_service:example.test"]));
    assert_eq!(body["initial_state"][0]["content"]["join_rule"], "invite");
    assert!(body.get("room_alias_name").is_none());
    let (_, _, body) = MatrixOperation::Create(
        input(Kind::Space),
        "@owner:example.test".into(),
        "@_hagency_service:example.test".into(),
    )
    .request()
    .unwrap();
    assert_eq!(body.unwrap()["creation_content"]["type"], "m.space");
    for bad in [
        "!room/escape:example.test",
        "!room?query",
        "!room#fragment",
        "!room\\escape",
        "room:example.test",
    ] {
        assert!(room_id(bad).is_err());
    }
    let (_, path, _) = MatrixOperation::State("!room%2F:example.test".into())
        .request()
        .unwrap();
    assert!(path.contains("%252F"));
    assert!(serde_json::from_value::<Input>(json!({"commandId":"x","kind":"room","name":"x","projectId":"p","spaceId":"!s:e","accessToken":"bad"})).is_err());
    let mut bad = input(Kind::Room);
    bad.project_id = None;
    assert!(validate_input(&bad).is_err());
}
#[test]
fn recovery_requires_exact_owner_creator_marker_and_private_rules() {
    let command = command();
    let good = state(&command);
    assert!(created_by(&good, &command));
    for (index, key, value) in [
        (0, "sender", json!("@other:example.test")),
        (1, "sender", json!("@other:example.test")),
    ] {
        let mut wrong = good.clone();
        wrong[index][key] = value;
        assert!(!created_by(&wrong, &command));
    }
    let mut wrong = good.clone();
    wrong[0]["content"]["creator"] = json!("@other:example.test");
    assert!(!created_by(&wrong, &command));
    let mut wrong = good.clone();
    wrong[1]["content"]["commandId"] = json!("another");
    assert!(!created_by(&wrong, &command));
    let mut wrong = good.clone();
    wrong[2]["content"]["join_rule"] = json!("public");
    assert!(!created_by(&wrong, &command));
    let mut wrong = good.clone();
    wrong.as_array_mut().unwrap().push(good[1].clone());
    assert!(!created_by(&wrong, &command));
    let mut wrong = good.clone();
    wrong[0]["content"]["type"] = json!("m.space");
    assert!(!created_by(&wrong, &command));
    // Modern room versions omit content.creator; immutable create event sender remains mandatory.
    let mut modern = good.clone();
    modern[0]["content"]
        .as_object_mut()
        .unwrap()
        .remove("creator");
    assert!(created_by(&modern, &command));
    assert_eq!(
        service_identity(
            &json!({"serverName":"example.test","serviceMxid":"@_hagency_service:example.test"}),
            &command.owner
        )
        .unwrap(),
        "@_hagency_service:example.test"
    );
    assert!(
        service_identity(
            &json!({"serverName":"example.test","serviceMxid":"@untrusted:example.test"}),
            &command.owner
        )
        .is_err()
    );
    assert!(service_identity(&json!({"serverName":"elsewhere.test","serviceMxid":"@_hagency_service:elsewhere.test"}),&command.owner).is_err());
}
#[tokio::test]
async fn matrix_calls_use_only_current_oauth_grant_and_fail_closed_after_expiry() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", socket.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut stream, _) = socket.accept().await.unwrap();
        let mut wire = vec![];
        loop {
            let mut bytes = [0; 1024];
            let n = stream.read(&mut bytes).await.unwrap();
            wire.extend_from_slice(&bytes[..n]);
            if wire.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        let wire = String::from_utf8(wire).unwrap();
        assert!(wire.starts_with("GET /_matrix/client/v3/joined_rooms HTTP/1.1"));
        assert!(
            wire.to_ascii_lowercase()
                .contains("authorization: bearer oauth-matrix-user")
        );
        assert!(!wire.contains("server-owner-session"));
        let body = r#"{"joined_rooms":["!room:example.test"]}"#;
        stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",body.len(),body).as_bytes()).await.unwrap();
    });
    let login = ServerLogin::new(None).unwrap();
    let key = session_key("browser-cookie");
    let now = Instant::now();
    login.sessions.lock().await.insert(
        key.clone(),
        super::super::RemoteSession {
            matrix_source: None,
            token: "server-owner-session".into(),
            oauth_token: "oauth-matrix-user".into(),
            user_id: "user-id".into(),
            device: None,
            refresh_token: None,
            oauth_expires: now + Duration::from_secs(300),
            authorized_until: now + Duration::from_secs(30),
            invalidated: false,
            binding: super::super::Binding {
                origin: base,
                client_id: "native-client".into(),
                installation_id: "a".repeat(32),
                name: "Owner".into(),
                owner: Some("@owner:example.test".into()),
                subject: Some("subject".into()),
            },
            checked: now,
            expires: now + Duration::from_secs(300),
        },
    );
    assert_eq!(
        login
            .matrix_api("browser-cookie", MatrixOperation::JoinedRooms)
            .await
            .unwrap()
            .value["joined_rooms"][0],
        "!room:example.test"
    );
    task.await.unwrap();
    login.sessions.lock().await.get_mut(&key).unwrap().expires = Instant::now();
    assert!(
        login
            .matrix_api("browser-cookie", MatrixOperation::JoinedRooms)
            .await
            .is_err()
    );
    assert!(
        login
            .matrix_api("different-cookie", MatrixOperation::JoinedRooms)
            .await
            .is_err()
    );
}
pub(super) async fn fixture(
    steps: Vec<(&'static str, Value, Option<Value>)>,
) -> (ServerLogin, tokio::task::JoinHandle<()>) {
    fixture_with_hook(steps, |_| {}).await
}
pub(super) async fn fixture_with_hook(
    steps: Vec<(&'static str, Value, Option<Value>)>,
    hook: impl Fn(&str) + Send + 'static,
) -> (ServerLogin, tokio::task::JoinHandle<()>) {
    fixture_with_status_and_hook(steps.into_iter().map(|(path, value, body)| (path, 200, value, body)).collect(), hook).await
}
pub(in crate::console::server_login) async fn fixture_with_status_and_hook(
    steps: Vec<(&'static str, u16, Value, Option<Value>)>,
    hook: impl Fn(&str) + Send + 'static,
) -> (ServerLogin, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", socket.local_addr().unwrap());
    let task = tokio::spawn(async move {
        for (expected, status, response, expected_body) in steps {
            let (mut stream, _) = socket.accept().await.unwrap();
            let mut wire = vec![];
            let header_end;
            loop {
                let mut bytes = [0; 1024];
                let n = stream.read(&mut bytes).await.unwrap();
                assert_ne!(n, 0);
                wire.extend_from_slice(&bytes[..n]);
                if let Some(end) = wire.windows(4).position(|w| w == b"\r\n\r\n") {
                    header_end = end + 4;
                    break;
                }
            }
            let headers = String::from_utf8(wire[..header_end].to_vec()).unwrap();
            assert_eq!(
                headers
                    .lines()
                    .next()
                    .unwrap()
                    .strip_suffix(" HTTP/1.1")
                    .unwrap(),
                expected
            );
            let owner = expected.contains("/hagency/api/v1/") && !expected.ends_with("/discovery");
            assert!(headers.to_ascii_lowercase().contains(if owner {
                "authorization: bearer owner-session"
            } else {
                "authorization: bearer matrix-oauth"
            }));
            let length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|n| n.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            while wire.len() < header_end + length {
                let mut bytes = [0; 1024];
                let n = stream.read(&mut bytes).await.unwrap();
                assert_ne!(n, 0);
                wire.extend_from_slice(&bytes[..n]);
            }
            if let Some(expected) = expected_body {
                assert_eq!(
                    serde_json::from_slice::<Value>(&wire[header_end..]).unwrap(),
                    expected
                );
            }
            hook(expected);
            let body = response.to_string();
            stream.write_all(format!("HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",body.len(),body).as_bytes()).await.unwrap();
        }
    });
    let login = ServerLogin::new(None).unwrap();
    let now = Instant::now();
    login.sessions.lock().await.insert(
        session_key("cookie"),
        super::super::RemoteSession {
            matrix_source: None,
            token: "owner-session".into(),
            oauth_token: "matrix-oauth".into(),
            user_id: "user".into(),
            device: None,
            refresh_token: None,
            oauth_expires: now + Duration::from_secs(300),
            authorized_until: now + Duration::from_secs(30),
            invalidated: false,
            binding: super::super::Binding {
                origin: base,
                client_id: "native".into(),
                installation_id: "i".repeat(32),
                name: "Owner".into(),
                owner: Some("@owner:example.test".into()),
                subject: Some("subject".into()),
            },
            checked: now,
            expires: now + Duration::from_secs(300),
        },
    );
    (login, task)
}
#[tokio::test]
async fn links_then_adopts_with_separate_credentials_and_never_overwrites_conflicting_child() {
    let c = command();
    let projects = json!({"projects":[{"id":"prj_1","spaceId":"!space:example.test"}]});
    let child = json!({"via":["example.test"],"suggested":false});
    let parent = json!({"via":["example.test"],"canonical":true});
    let (login,task)=fixture(vec![
 ("GET /hagency/api/v1/projects",projects.clone(),None),
 ("GET /_matrix/client/v3/rooms/!room:example.test/state",state(&c),None),
 ("GET /_matrix/client/v3/rooms/!space:example.test/state",json!([]),None),
 ("PUT /_matrix/client/v3/rooms/!space:example.test/state/m.space.child/!room:example.test",json!({"event_id":"$child"}),Some(child.clone())),
 ("PUT /_matrix/client/v3/rooms/!room:example.test/state/m.space.parent/!space:example.test",json!({"event_id":"$parent"}),Some(parent.clone())),
 ("POST /hagency/api/v1/projects/prj_1/rooms/adopt",json!({"roomId":"!room:example.test"}),Some(json!({"roomId":"!room:example.test"}))),
 ]).await;
    let mut command = c.clone();
    continue_known(&login, "cookie", &mut command, None)
        .await
        .unwrap();
    assert_eq!(command.phase, "complete");
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    let mut linked = state(&c);
    linked
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"m.space.parent","state_key":"!space:example.test","content":parent}));
    let (login, task) = fixture(vec![
        ("GET /hagency/api/v1/projects", projects.clone(), None),
        (
            "GET /_matrix/client/v3/rooms/!room:example.test/state",
            linked,
            None,
        ),
        (
            "GET /_matrix/client/v3/rooms/!space:example.test/state",
            json!([{"type":"m.space.child","state_key":"!room:example.test","content":child}]),
            None,
        ),
        (
            "POST /hagency/api/v1/projects/prj_1/rooms/adopt",
            json!({}),
            Some(json!({"roomId":"!room:example.test"})),
        ),
    ])
    .await;
    continue_known(&login, "cookie", &mut command, None)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    let (login,task)=fixture(vec![
 ("GET /hagency/api/v1/projects",projects,None),
 ("GET /_matrix/client/v3/rooms/!room:example.test/state",state(&c),None),
 ("GET /_matrix/client/v3/rooms/!space:example.test/state",json!([{"type":"m.space.child","state_key":"!room:example.test","content":{"via":["other.test"],"suggested":true}}]),None),
 ]).await;
    assert_eq!(
        continue_known(&login, "cookie", &mut command, None)
            .await
            .unwrap_err()
            .code,
        "matrix_space_child_conflict"
    );
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
}

#[test]
fn space_candidates_filter_real_membership_metadata_and_bound_pagination() {
    let ids = candidate_ids(
        &json!({"joined_rooms":["!c:test","!a:test","!b:test","!a:test"]}),
        Some("!a:test"),
    )
    .unwrap();
    assert_eq!(ids, vec!["!b:test", "!c:test"]);
    assert!(candidate_ids(&json!({"joined_rooms":vec!["!a:test";4097]}), None).is_err());
    let owner = "@owner:test";
    let projects = json!({"projects":[{"id":"prj_x","spaceId":"!s:test"}]});
    let mut state = json!([
        {"type":"m.room.create","state_key":"","content":{"type":"m.space"}},
        {"type":"m.room.member","state_key":owner,"content":{"membership":"join"}},
        {"type":"m.room.name","state_key":"","content":{"name":"Existing Project"}},
        {"type":"m.room.topic","state_key":"","content":{"topic":"Discussion"}}
    ]);
    let candidate = candidate_space("!s:test", &state, owner, &projects)
        .unwrap()
        .unwrap();
    assert_eq!(
        candidate,
        json!({"spaceId":"!s:test","name":"Existing Project","topic":"Discussion","projectId":"prj_x"})
    );
    state[1]["content"]["membership"] = json!("leave");
    assert!(
        candidate_space("!s:test", &state, owner, &projects)
            .unwrap()
            .is_none()
    );
    state[1]["content"]["membership"] = json!("join");
    state[0]["content"] = json!({});
    assert!(
        candidate_space("!s:test", &state, owner, &projects)
            .unwrap()
            .is_none()
    );
    assert!(candidate_space("!s:test", &json!({}), owner, &projects).is_err());
}

#[tokio::test]
async fn binding_repairs_missing_service_invitation_with_user_credentials_and_original_key() {
    let body = json!({"projectId":"prj_1","roomId":"!room:example.test","idempotencyKey":"original"});
    for member in [None, Some("invite"), Some("join")] {
        let mut state = json!([
            {"type":"m.room.member","state_key":"@owner:example.test","content":{"membership":"join"}},
            {"type":"m.room.power_levels","state_key":"","content":{"users":{"@owner:example.test":100},"invite":0}}
        ]);
        if let Some(member) = member { state.as_array_mut().unwrap().push(json!({"type":"m.room.member","state_key":"@_hagency_service:example.test","content":{"membership":member}})); }
        let mut steps = vec![
            ("POST /hagency/api/v1/agents/agt_1/bindings", 401, json!({"code":"service_invitation_denied"}), Some(body.clone())),
            ("GET /hagency/api/v1/discovery", 200, json!({"serverName":"example.test","serviceMxid":"@_hagency_service:example.test"}), None),
            ("GET /_matrix/client/v3/rooms/!room:example.test/state", 200, state, None),
        ];
        if member.is_none() {steps.push(("POST /_matrix/client/v3/rooms/!room:example.test/invite", 200, json!({}), Some(json!({"user_id":"@_hagency_service:example.test"}))));}
        steps.push(("POST /hagency/api/v1/agents/agt_1/bindings", 200, json!({"commandState":"pending"}), Some(body.clone())));
        let (login, task) = fixture_with_status_and_hook(steps, |_|{}).await;
        let reply = login.bind_agent_room("cookie", "agt_1".into(), "prj_1".into(), "!room:example.test".into(), "original".into()).await.unwrap();
        assert_eq!(reply.value["commandState"], "pending");
        tokio::time::timeout(Duration::from_secs(5), task).await.unwrap().unwrap();
    }
}
#[tokio::test]
async fn binding_does_not_invite_on_owner_policy_denial_or_uncertain_response() {
    for (status, code) in [(401,"agent_creation_denied"),(401,"room_membership_required"),(503,"server_unavailable")] {
        let (login, task) = fixture_with_status_and_hook(vec![("POST /hagency/api/v1/agents/agt_1/bindings",status,json!({"code":code}),Some(json!({"projectId":"prj_1","roomId":"!room:example.test","idempotencyKey":"original"})))], |_|{}).await;
        let error = login.bind_agent_room("cookie", "agt_1".into(), "prj_1".into(), "!room:example.test".into(), "original".into()).await.err().expect("expected refusal");
        assert_eq!(error.code,code);
        tokio::time::timeout(Duration::from_secs(5),task).await.unwrap().unwrap();
    }
}
