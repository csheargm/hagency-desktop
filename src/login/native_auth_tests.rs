//! User flow: native password sign-in and sign-up against the fixture
//! server — no dynamic client registration, no browser, real SDK token
//! exchange and whoami.
use super::*;

async fn sign_in(register: bool) -> (Fixture, Client) {
    let fixture = Fixture::new();
    let client = Client::builder().homeserver_url(&fixture.origin).handle_refresh_tokens().build().await.unwrap();
    let origin = fixture.origin.clone();
    let config = crate::login::native_auth::adopt_native_client(&client, &origin).await.expect("native API offered");
    assert!(crate::login::native_auth::uses_native_client(&client, &config));
    crate::login::native_auth::password_sign_in(&client, origin, "fixture".into(), "pw".into(), register).await.unwrap();
    (fixture, client)
}

#[tokio::test]
async fn native_password_sign_in_needs_no_browser_or_client_registration() {
    for register in [false, true] {
        let (fixture, client) = sign_in(register).await;
        assert_eq!(client.user_id().unwrap().as_str(), "@fixture:localhost");
        assert!(client.oauth().full_session().is_some());
        let routes: Vec<String> = fixture.requests.lock().unwrap().iter().map(|(r, _)| r.clone()).collect();
        let endpoint = if register { "/native/register " } else { "/native/login " };
        assert!(routes.iter().any(|r| r.contains(endpoint)), "{routes:?}");
        assert!(!routes.iter().any(|r| r.contains("/auth/register ")), "no dynamic registration");
        assert!(!routes.iter().any(|r| r.contains("/api/v1/auth/login ")), "no cookie login");
        assert!(routes.iter().any(|r| r.contains("/auth/token ")));
    }
}

#[tokio::test]
async fn servers_without_the_native_api_keep_the_older_flow() {
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", socket.local_addr().unwrap());
    drop(socket);
    let client = Client::builder().homeserver_url(&origin).build().await.unwrap();
    assert!(crate::login::native_auth::adopt_native_client(&client, &origin).await.is_none());
    assert!(client.oauth().client_id().is_none());
}
