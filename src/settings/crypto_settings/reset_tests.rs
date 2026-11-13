//! The identity reset against a homeserver that keeps keys, a backup and account data, and
//! confirms a replaced identity with the account password, as a password homeserver does.
use super::*;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex as StdMutex},
};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate, matchers::any};

const USER: &str = "@alice:example.org";
const PASSWORD: &str = "synthetic-account-password";

#[derive(Default)]
struct Home {
    devices: BTreeMap<String, Value>,
    master: Option<Value>,
    self_signing: Option<Value>,
    user_signing: Option<Value>,
    backup: Option<&'static str>,
    account_data: BTreeMap<String, Value>,
    /// What changed on the account, in order.
    changes: Vec<&'static str>,
    /// Refuse this many signature uploads: the request that follows a replaced identity.
    refuse_signatures: u32,
    /// Every request, in order, for a failing test's report.
    requests: Vec<String>,
}
#[derive(Clone, Default)]
struct Homeserver(Arc<StdMutex<Home>>);
fn merge_signatures(target: &mut Value, signed: &Value) {
    let Some(new) = signed["signatures"].as_object() else {
        return;
    };
    for (user, keys) in new {
        for (key, signature) in keys.as_object().into_iter().flatten() {
            target["signatures"][user][key] = signature.clone();
        }
    }
}
impl Respond for Homeserver {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let mut home = self.0.lock().unwrap();
        let path = request.url.path().to_owned();
        let method = request.method.as_str().to_owned();
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        let line = format!("{method} {}", path.rsplit("/v3/").next().unwrap_or(&path));
        home.requests.push(line);
        let ok = |value: Value| ResponseTemplate::new(200).set_body_json(value);
        let missing = || {
            ResponseTemplate::new(404).set_body_json(json!({"errcode":"M_NOT_FOUND","error":"Not found"}))
        };
        if path.ends_with("/sync") {
            return ok(json!({"next_batch":"s1"}));
        }
        if path.ends_with("/keys/upload") {
            if let Some(device) = body.get("device_keys").filter(|keys| keys.is_object()) {
                let id = device["device_id"].as_str().unwrap().to_owned();
                home.devices.insert(id, device.clone());
            }
            return ok(json!({"one_time_key_counts":{"signed_curve25519":50}}));
        }
        if path.ends_with("/keys/device_signing/upload") {
            if home.master.is_some() {
                // An existing identity is replaced only with the account password.
                let Some(auth) = body.get("auth").filter(|auth| auth.is_object()) else {
                    return ResponseTemplate::new(401).set_body_json(json!({
                        "flows":[{"stages":["m.login.password"]}],"params":{},"session":"uia-1"
                    }));
                };
                if auth["type"] != "m.login.password"
                    || auth["session"] != "uia-1"
                    || auth["identifier"]["user"] != USER
                    || auth["password"] != PASSWORD
                {
                    return ResponseTemplate::new(403).set_body_json(
                        json!({"errcode":"M_FORBIDDEN","error":"Wrong username or password."}),
                    );
                }
                home.changes.push("identity replaced");
            }
            home.master = Some(body["master_key"].clone());
            home.self_signing = Some(body["self_signing_key"].clone());
            home.user_signing = Some(body["user_signing_key"].clone());
            return ok(json!({}));
        }
        if path.ends_with("/keys/signatures/upload") {
            if home.refuse_signatures > 0 {
                home.refuse_signatures -= 1;
                return ResponseTemplate::new(403)
                    .set_body_json(json!({"errcode":"M_FORBIDDEN","error":"Refused"}));
            }
            for signed in body[USER].as_object().into_iter().flatten().map(|entry| entry.1) {
                if let Some(device) = signed["device_id"].as_str() {
                    if let Some(known) = home.devices.get_mut(device) {
                        merge_signatures(known, signed);
                    }
                } else if let Some(master) = home.master.as_mut() {
                    merge_signatures(master, signed);
                }
            }
            return ok(json!({"failures":{}}));
        }
        if path.ends_with("/keys/query") {
            let mut reply = json!({"failures":{},"device_keys":{USER: home.devices}});
            for (name, key) in [
                ("master_keys", &home.master),
                ("self_signing_keys", &home.self_signing),
                ("user_signing_keys", &home.user_signing),
            ] {
                if let Some(key) = key {
                    reply[name] = json!({USER: key});
                }
            }
            return ok(reply);
        }
        if path.ends_with("/room_keys/version") && method == "GET" {
            return match home.backup {
                Some(version) => ok(json!({
                    "algorithm":"m.megolm_backup.v1.curve25519-aes-sha2",
                    "auth_data":{"public_key":"hsDHTo1KmG2yXY7HSvOCF7Zb8NLQgNR5aZjRUZk2xQ8","signatures":{}},
                    "count":3,"etag":"1","version":version
                })),
                None => missing(),
            };
        }
        if path.contains("/room_keys/version/") && method == "DELETE" {
            home.backup = None;
            home.changes.push("backup deleted");
            return ok(json!({}));
        }
        if let Some((_, kind)) = path.split_once("/account_data/") {
            if method == "PUT" {
                if kind == "m.secret_storage.default_key" {
                    home.changes.push("recovery key retired");
                }
                home.account_data.insert(kind.to_owned(), body);
                return ok(json!({}));
            }
            return home.account_data.get(kind).cloned().map(ok).unwrap_or_else(missing);
        }
        missing()
    }
}
async fn device(server: &MockServer, device: &str) -> Client {
    let client = Client::builder()
        .http_client(
            matrix_sdk::reqwest::Client::builder()
                .no_proxy()
                .build()
                .unwrap(),
        )
        .homeserver_url(server.uri())
        .server_versions([matrix_sdk::ruma::api::MatrixVersion::V1_11])
        .build()
        .await
        .unwrap();
    client
        .matrix_auth()
        .restore_session(
            serde_json::from_value(
                json!({"access_token":"fixture","user_id":USER,"device_id":device}),
            )
            .unwrap(),
            Default::default(),
        )
        .await
        .unwrap();
    // A signed-in device has uploaded its own keys long before any reset, so the signature a
    // new identity makes for it travels in a request of its own.
    client
        .sync_once(matrix_sdk::config::SyncSettings::default().timeout(std::time::Duration::ZERO))
        .await
        .unwrap();
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .any(|request| request.url.path().ends_with("/keys/upload")
                && request.body_json::<Value>().is_ok_and(|body| body["device_keys"]["device_id"] == device)),
        "the device's keys are on the server before the test starts"
    );
    client
}

#[test]
fn a_reset_changes_nothing_until_the_server_confirms_then_replaces_identity_backup_and_recovery() {
    const NAME: &str = "settings::crypto_settings::backend::reset_tests::a_reset_changes_nothing_until_the_server_confirms_then_replaces_identity_backup_and_recovery";
    if std::env::var("HAGENCY_CRYPTO_UI_TEST").as_deref() != Ok(NAME) {
        let profile = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
            .env("HAGENCY_CRYPTO_UI_TEST", NAME)
            .env("HAGENCY_DESKTOP_DATA_DIR", profile.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        return;
    }
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let home = Homeserver::default();
        let server = MockServer::start().await;
        Mock::given(any()).respond_with(home.clone()).mount(&server).await;

        // The account's identity and backup were made on a device this person no longer has.
        let lost = device(&server, "LOST").await;
        lost.encryption().bootstrap_cross_signing(None).await.unwrap();
        let old_master = {
            let mut home = home.0.lock().unwrap();
            home.backup = Some("1");
            assert!(home.changes.is_empty());
            home.master.clone().unwrap()
        };

        let client = device(&server, "HERE").await;
        crate::settings::crypto_settings::test_support::install_link_client(Some(client.clone()));
        let pin = SessionPin::capture(&client).unwrap();
        let before = snapshot(&client, &pin).await.unwrap();
        assert!(before.identity && !before.verified && !before.private_keys);
        assert!(before.backup && !before.backup_connected);
        assert!(!before.can_prepare(), "set-up would replace keys this device cannot read");
        assert!(before.can_reset());

        // No password, then a wrong one: the server refuses, and the account stays as it was.
        for wrong in ["", "not-the-password"] {
            let refused = perform(
                &client,
                &pin,
                1,
                Operation::Reset(Zeroizing::new(wrong.to_owned())),
            )
            .await
            .unwrap_err()
            .to_string();
            assert!(refused.ends_with("Nothing was changed."), "{refused}");
            let home = home.0.lock().unwrap();
            assert_eq!(home.master.as_ref(), Some(&old_master));
            assert_eq!(home.backup, Some("1"));
            assert!(home.changes.is_empty(), "{:?}", home.changes);
        }
        assert!(Plan::load(&root(), &client).unwrap().is_none());

        let notice = perform(
            &client,
            &pin,
            2,
            Operation::Reset(Zeroizing::new(PASSWORD.to_owned())),
        )
        .await
        .unwrap();
        {
            let home = home.0.lock().unwrap();
            assert_ne!(home.master.as_ref(), Some(&old_master));
            assert_eq!(home.backup, None);
            assert_eq!(
                home.changes,
                ["identity replaced", "backup deleted", "recovery key retired"],
                "nothing is deleted before the server has confirmed the new identity"
            );
            assert_eq!(home.account_data["m.secret_storage.default_key"], json!({}));
        }
        let after = snapshot(&client, &pin).await.unwrap();
        assert!(after.identity && after.verified && after.private_keys);
        assert!(!after.backup && after.default_key.is_none());
        assert!(!after.can_reset(), "a working identity is not offered a reset");
        if file_picker_supported() {
            assert_eq!(
                notice,
                Some("Encryption identity reset. Save your new recovery key, then enable recovery.")
            );
            assert!(after.pending && after.key_loaded && !after.key_saved);
            let plan = Plan::load(&root(), &client).unwrap().unwrap();
            assert_eq!(plan.master, master(&identity(&client, &pin).await.unwrap()));
        } else {
            assert_eq!(notice, Some("Encryption identity reset. Set up recovery next."));
        }
        // The lost device, were it to come back, no longer holds the account's identity.
        let lost_view = lost
            .encryption()
            .request_user_identity(lost.user_id().unwrap())
            .await
            .unwrap()
            .unwrap();
        assert!(!lost_view.is_verified());
        crate::settings::crypto_settings::test_support::install_link_client(None);
    });
}

#[test]
fn a_reset_cut_off_after_the_new_identity_arrived_is_not_reported_as_unchanged() {
    const NAME: &str = "settings::crypto_settings::backend::reset_tests::a_reset_cut_off_after_the_new_identity_arrived_is_not_reported_as_unchanged";
    if std::env::var("HAGENCY_CRYPTO_UI_TEST").as_deref() != Ok(NAME) {
        let profile = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
            .env("HAGENCY_CRYPTO_UI_TEST", NAME)
            .env("HAGENCY_DESKTOP_DATA_DIR", profile.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        return;
    }
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        // The new identity is accepted; the signatures that follow it are refused, `refused`
        // times. Returns the fake server, the device and what its first reset said.
        async fn cut_off(refused: u32) -> (Homeserver, MockServer, Client, SessionPin, Result<Option<&'static str>>) {
            let home = Homeserver::default();
            let server = MockServer::start().await;
            Mock::given(any()).respond_with(home.clone()).mount(&server).await;
            let lost = device(&server, "LOST").await;
            lost.encryption().bootstrap_cross_signing(None).await.unwrap();
            let old_master = {
                let mut home = home.0.lock().unwrap();
                home.backup = Some("1");
                home.refuse_signatures = refused;
                home.master.clone().unwrap()
            };
            let client = device(&server, "HERE").await;
            crate::settings::crypto_settings::test_support::install_link_client(Some(client.clone()));
            let pin = SessionPin::capture(&client).unwrap();
            let outcome = perform(
                &client,
                &pin,
                1,
                Operation::Reset(Zeroizing::new(PASSWORD.to_owned())),
            )
            .await;
            {
                let home = home.0.lock().unwrap();
                assert_ne!(home.master.as_ref(), Some(&old_master));
                // The old backup belongs to an identity that is gone: it is removed all the same.
                assert_eq!(
                    home.changes,
                    ["identity replaced", "backup deleted", "recovery key retired"]
                );
            }
            (home, server, client, pin, outcome)
        }

        // Refused once: the device's signature is sent again and the reset completes.
        let (_home, _server, client, pin, outcome) = cut_off(1).await;
        assert!(outcome.unwrap().is_some_and(|notice| notice.starts_with("Encryption identity reset.")));
        let after = snapshot(&client, &pin).await.unwrap();
        assert!(after.verified && after.private_keys && !after.can_reset());

        // Refused again: the server holds a new identity that has not signed this device.
        let (home, _server, client, pin, outcome) = cut_off(2).await;
        assert_eq!(
            outcome.map_err(|error| error.to_string()),
            Err(UNCONFIRMED_RESET.to_owned()),
            "{:#?}",
            home.0.lock().unwrap().requests
        );
        let between = snapshot(&client, &pin).await.unwrap();
        assert!(
            !between.verified && between.can_reset(),
            "a device that only its own store calls signed can still be reset"
        );
        assert!(!between.pending, "no recovery key is made for an unconfirmed identity");
        // Pressed again, as the message says, it completes.
        let notice = perform(
            &client,
            &pin,
            2,
            Operation::Reset(Zeroizing::new(PASSWORD.to_owned())),
        )
        .await
        .unwrap();
        assert!(notice.is_some_and(|notice| notice.starts_with("Encryption identity reset.")));
        let after = snapshot(&client, &pin).await.unwrap();
        assert!(after.verified && after.private_keys && !after.can_reset());
        crate::settings::crypto_settings::test_support::install_link_client(None);
    });
}
