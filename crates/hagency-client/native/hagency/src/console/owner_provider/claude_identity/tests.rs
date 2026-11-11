use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const ACCOUNT: &str = "01234567-89ab-cdef-0123-456789abcdef";
const ORG: &str = "89abcdef-0123-4567-89ab-cdef01234567";

fn status() -> Value {
    json!({"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty",
        "email":"owner@example.test","orgId":ORG})
}
fn account_profile() -> Value {
    json!({"account":{"uuid":ACCOUNT,"email":"owner@example.test"},"organization":{"uuid":ORG}})
}

#[cfg(unix)]
fn fixture(before: Value, after: Value) -> (tempfile::TempDir, ProviderPaths, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let home = root.path().canonicalize().unwrap();
    let config = home.join("native-config");
    hagency_store::private::directory(&config).unwrap();
    let binary = home.join("claude-fixture");
    let script = format!(
        "#!/usr/bin/env python3\nimport json,sys,pathlib\nassert sys.argv[1:]==['auth','status','--json']\np=pathlib.Path({})\na={}\nb={}\nif p.exists():\n print(b)\nelse:\n p.write_text('observed')\n print(a)\n",
        serde_json::to_string(&home.join("read-status")).unwrap(),
        serde_json::to_string(&before.to_string()).unwrap(),
        serde_json::to_string(&after.to_string()).unwrap()
    );
    std::fs::write(&binary, script).unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    (
        root,
        ProviderPaths {
            home,
            codex_home: config,
            credential_ref: "fixture-only".into(),
            shared: true,
        },
        binary,
    )
}

async fn endpoint(code: &str, extra: &str, body: Vec<u8>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/api/oauth/profile",
        listener.local_addr().unwrap()
    );
    let header = format!(
        "HTTP/1.1 {code}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        loop {
            let mut bytes = [0; 1024];
            let n = stream.read(&mut bytes).await.unwrap();
            assert!(n > 0 && request.len() + n <= 8192);
            request.extend_from_slice(&bytes[..n]);
            if request.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        let request = String::from_utf8(request).unwrap();
        assert!(request.starts_with("GET /api/oauth/profile HTTP/1.1"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer offline-fixture-only\r\n")
        );
        stream.write_all(header.as_bytes()).await.unwrap();
        // A bounded reader can close as soon as a large Content-Length arrives.
        let _ = stream.write_all(&body).await;
    });
    (url, task)
}

#[tokio::test]
#[cfg(unix)]
async fn claude_identity_requires_authenticated_uuid_and_unchanged_native_login() {
    for mode in [
        "valid",
        "switch",
        "wrong-email",
        "wrong-org",
        "missing-id",
        "expired",
    ] {
        let mut after = status();
        let mut observed = account_profile();
        match mode {
            "switch" => after["email"] = json!("another@example.test"),
            "wrong-email" => observed["account"]["email"] = json!("another@example.test"),
            "wrong-org" => observed["organization"]["uuid"] = json!(ACCOUNT),
            "missing-id" => observed["account"]["uuid"] = Value::Null,
            _ => {}
        }
        let (_root, paths, binary) = fixture(status(), after);
        let (url, server) = endpoint("200 OK", "", serde_json::to_vec(&observed).unwrap()).await;
        let deadline = if mode == "expired" {
            Instant::now()
        } else {
            Instant::now() + Duration::from_secs(20)
        };
        let result = observe_selected(&paths, &binary, deadline, &url, async {
            Ok(Token("offline-fixture-only".into()))
        })
        .await;
        assert_eq!(result.is_ok(), mode == "valid", "{mode}");
        if mode == "expired" {
            server.abort();
        } else {
            server.await.unwrap();
        }
    }
}

#[cfg(unix)]
fn api_status(method: &str) -> Value {
    json!({"loggedIn":true,"authMethod":method,"apiProvider":"firstParty"})
}
#[cfg(unix)]
fn write_settings(paths: &ProviderPaths, value: &Value, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let path = paths.codex_home.join("settings.json");
    std::fs::write(&path, value.to_string()).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
}
#[cfg(unix)]
async fn observe_api(paths: &ProviderPaths, binary: &std::path::Path) -> Result<AccountBinding, ProviderError> {
    observe_selected(paths, binary, Instant::now() + Duration::from_secs(20), PROFILE, async {
        panic!("an API credential never touches the claude.ai sign-in")
    })
    .await
}

#[tokio::test]
#[cfg(unix)]
async fn claude_identity_binds_an_api_credential_from_the_settings_file() {
    let token = json!({"env":{"ANTHROPIC_BASE_URL":"https://gateway.example/","ANTHROPIC_AUTH_TOKEN":"gateway-token",
        "ANTHROPIC_DEFAULT_SONNET_MODEL":"open-model-a","ANTHROPIC_CUSTOM_HEADERS":"x: y"},"permissions":{}});
    let (_root, paths, binary) = fixture(api_status("oauth_token"), api_status("oauth_token"));
    write_settings(&paths, &token, 0o644);
    let binding = observe_api(&paths, &binary).await.unwrap();
    assert!(binding.is_api_credential());
    let expected = AccountBinding::from_api_credential(
        Some("https://gateway.example"), "gateway-token", false, Instant::now() + Duration::from_secs(20)).unwrap();
    assert_eq!(binding.provider_digest(), expected.provider_digest());

    let key = json!({"env":{"ANTHROPIC_API_KEY":"sk-ant-api03-example"}});
    let (_root, paths, binary) = fixture(api_status("api_key"), api_status("api_key"));
    write_settings(&paths, &key, 0o600);
    let binding = observe_api(&paths, &binary).await.unwrap();
    let expected = AccountBinding::from_api_credential(
        None, "sk-ant-api03-example", true, Instant::now() + Duration::from_secs(20)).unwrap();
    assert_eq!(binding.provider_digest(), expected.provider_digest());
}

#[tokio::test]
#[cfg(unix)]
async fn claude_identity_refuses_an_api_credential_it_cannot_take_from_the_settings_file() {
    let gateway = json!({"env":{"ANTHROPIC_BASE_URL":"https://gateway.example","ANTHROPIC_AUTH_TOKEN":"gateway-token"}});
    for (mode, method, settings, permissions) in [
        // The CLI says "API key" but the file holds no key (a helper or a shell variable).
        ("key-elsewhere", "api_key", json!({"env":{"ANTHROPIC_BASE_URL":"https://gateway.example"}}), 0o644),
        ("no-env", "oauth_token", json!({"permissions":{}}), 0o644),
        // Both set: the CLI would use the bearer token, not the key it reported.
        ("both", "api_key", json!({"env":{"ANTHROPIC_API_KEY":"sk-ant-api03-example","ANTHROPIC_AUTH_TOKEN":"t"}}), 0o644),
        // A claude.ai sign-in token, or a bearer token for Anthropic's own API.
        ("subscription-token", "oauth_token", json!({"env":{"ANTHROPIC_BASE_URL":"https://gateway.example","ANTHROPIC_AUTH_TOKEN":"sk-ant-oat01-example"}}), 0o644),
        ("bearer-anthropic", "oauth_token", json!({"env":{"ANTHROPIC_AUTH_TOKEN":"some-token"}}), 0o644),
        ("claude-oauth-variable", "oauth_token", json!({"env":{"CLAUDE_CODE_OAUTH_TOKEN":"sk-ant-oat01-example"}}), 0o644),
        ("bad-endpoint", "oauth_token", json!({"env":{"ANTHROPIC_BASE_URL":"file:///etc","ANTHROPIC_AUTH_TOKEN":"gateway-token"}}), 0o644),
        ("not-a-string", "oauth_token", json!({"env":{"ANTHROPIC_BASE_URL":"https://gateway.example","ANTHROPIC_AUTH_TOKEN":7}}), 0o644),
        ("writable-by-others", "oauth_token", gateway.clone(), 0o666),
    ] {
        let (_root, paths, binary) = fixture(api_status(method), api_status(method));
        write_settings(&paths, &settings, permissions);
        assert!(observe_api(&paths, &binary).await.is_err(), "{mode}");
    }
    // No settings file, a symlink in its place, and a sign-in that changes meanwhile.
    let (_root, paths, binary) = fixture(api_status("oauth_token"), api_status("oauth_token"));
    assert!(observe_api(&paths, &binary).await.is_err());
    let (root, paths, binary) = fixture(api_status("oauth_token"), api_status("oauth_token"));
    let real = root.path().join("elsewhere.json");
    std::fs::write(&real, gateway.to_string()).unwrap();
    std::os::unix::fs::symlink(&real, paths.codex_home.join("settings.json")).unwrap();
    assert!(observe_api(&paths, &binary).await.is_err());
    let (_root, paths, binary) = fixture(api_status("oauth_token"), api_status("api_key"));
    write_settings(&paths, &gateway, 0o644);
    assert!(observe_api(&paths, &binary).await.is_err());
}

#[tokio::test]
#[cfg(unix)]
async fn claude_identity_never_reads_credentials_for_other_backends_or_unidentified_login() {
    for mode in ["api-key", "gateway", "missing-org", "not-logged-in"] {
        let mut value = status();
        match mode {
            "api-key" => value["authMethod"] = json!("api_key"),
            "gateway" => value["apiProvider"] = json!("gateway"),
            "missing-org" => value["orgId"] = Value::Null,
            _ => value["loggedIn"] = json!(false),
        }
        let (_root, paths, binary) = fixture(value.clone(), value);
        let result = observe_selected(
            &paths,
            &binary,
            Instant::now() + Duration::from_secs(20),
            PROFILE,
            async { panic!("unidentified login must not access credentials") },
        )
        .await;
        assert!(result.is_err());
    }
}

#[tokio::test]
async fn claude_identity_profile_never_follows_redirects_and_bounds_errors_and_body() {
    for (code, header, body) in [
        (
            "302 Found",
            "Location: http://127.0.0.1:9/never-send-token\r\n",
            b"{}".to_vec(),
        ),
        ("401 Unauthorized", "", b"private provider error".to_vec()),
        ("200 OK", "", vec![b' '; MAX_BYTES + 1]),
        ("200 OK", "", b"not json".to_vec()),
    ] {
        let (url, server) = endpoint(code, header, body).await;
        let result = profile(
            &client().unwrap(),
            &url,
            &Token("offline-fixture-only".into()),
        )
        .await;
        assert!(result.is_err());
        let error = result.err().unwrap();
        assert_eq!(error.code, "provider_identity_unavailable");
        server.await.unwrap();
    }
}

#[test]
fn claude_identity_native_token_requires_profile_scope_and_bounded_safe_header() {
    for mode in [
        "valid",
        "missing-scope",
        "control",
        "oversized",
        "wrong-type",
    ] {
        let mut value = json!({"claudeAiOauth":{"accessToken":"offline-fixture-only","scopes":["user:profile"]}});
        match mode {
            "missing-scope" => value["claudeAiOauth"]["scopes"] = json!(["user:inference"]),
            "control" => value["claudeAiOauth"]["accessToken"] = json!("offline\nfixture"),
            "oversized" => value["claudeAiOauth"]["accessToken"] = json!("x".repeat(8193)),
            "wrong-type" => value["claudeAiOauth"]["accessToken"] = json!(true),
            _ => {}
        }
        assert_eq!(
            Token::parse(&serde_json::to_vec(&value).unwrap()).is_ok(),
            mode == "valid"
        );
    }
    assert!(Token::parse(&vec![b' '; MAX_BYTES + 1]).is_err());
}

#[test]
#[cfg(unix)]
fn claude_identity_file_read_is_readonly_private_regular_and_nofollow() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let root = tempfile::tempdir().unwrap();
    let config = root.path();
    let path = config.join(".credentials.json");
    let bytes = serde_json::to_vec(&json!({"claudeAiOauth":{
        "accessToken":"offline-fixture-only","scopes":["user:profile"]
    }}))
    .unwrap();
    std::fs::write(&path, &bytes).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
    assert!(file_token(config).is_ok());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o400
    );

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(file_token(config).is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
    let target = config.join("native-credential");
    std::fs::rename(&path, &target).unwrap();
    symlink(&target, &path).unwrap();
    assert!(file_token(config).is_err());
    std::fs::remove_file(&path).unwrap();
    std::fs::hard_link(&target, &path).unwrap();
    assert!(file_token(config).is_err());
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(file_token(config).is_err());
    std::fs::remove_dir(&path).unwrap();
    assert!(
        std::process::Command::new("mkfifo")
            .args(["-m", "600"])
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    assert!(file_token(config).is_err());
}

#[test]
#[cfg(target_os = "macos")]
fn claude_identity_keychain_locator_matches_native_config_normalization() {
    let a = keychain_service(Path::new("/private/Caf\u{00e9}/claude")).unwrap();
    let b = keychain_service(Path::new("/private/Cafe\u{0301}/claude")).unwrap();
    assert_eq!(a, b);
    assert_ne!(
        a,
        keychain_service(Path::new("/private/another/claude")).unwrap()
    );
    assert!(a.starts_with("Claude Code-credentials-"));
    assert_eq!(a.len(), "Claude Code-credentials-".len() + 8);
}
