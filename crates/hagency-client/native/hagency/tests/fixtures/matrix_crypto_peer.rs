//! Independent recipient and protocol fixture. This module never opens, seeds,
//! or obtains a reference to the service SDK. Its server query is populated only
//! by the recipient's own public material and actual service upload requests.
#![allow(dead_code)]
use matrix_sdk_crypto::{
    DecryptionSettings, EncryptionSyncChanges, OlmMachine, TrustRequirement, UserIdentity,
    types::requests::AnyOutgoingRequest,
};
use ruma::{RoomId, api::client::keys::get_keys, device_id, serde::Raw, user_id};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const SENDER: &str = "@worker:example.test";
pub const DEVICE: &str = "DEVICE_1";
pub const HUMAN: &str = "@owner:example.test";
pub const HUMAN_DEVICE: &str = "HUMAN";

pub struct Peer {
    sender: ruma::OwnedUserId,
    device: String,
    pub query: Value,
    pub writes: Vec<(String, Value)>,
    pub claims: usize,
    pub shares: usize,
    pub events: Vec<Value>,
    /// State events written by the real private approval-room marker publisher.
    pub approval_markers: Vec<Value>,
    human: OlmMachine,
    one_time: BTreeMap<String, Value>,
    sender_master: Option<Value>,
    recipient_trusted: bool,
    /// The owner's other devices (ADR-183 B): each is its own independent
    /// machine with its own one-time keys; `signed` names the ones the
    /// owner's self-signing key has signed. A device the fixture parks is
    /// absent from `/keys/query` until restored.
    extra: BTreeMap<String, OlmMachine>,
    extra_one_time: BTreeMap<String, BTreeMap<String, Value>>,
    signed: BTreeSet<String>,
    parked: BTreeMap<String, Value>,
    /// Every owner device a room key was shared with, across all shares.
    pub shared_devices: BTreeSet<String>,
}

impl Peer {
    pub async fn new() -> Self {
        Self::for_sender(SENDER, DEVICE).await
    }
    pub async fn for_sender(sender: &str, sender_device: &str) -> Self {
        let human = OlmMachine::new(user_id!("@owner:example.test"), device_id!("HUMAN")).await;
        // Provision only the independent recipient. None of these original
        // requests or private identity handles can reach the service process.
        let bootstrap = human.bootstrap_cross_signing(false).await.unwrap();
        let upload = bootstrap.upload_keys_req.as_ref().unwrap();
        let AnyOutgoingRequest::KeysUpload(upload) = upload.request() else {
            panic!("recipient bootstrap did not generate its real device/OTKs")
        };
        assert!(!upload.one_time_keys.is_empty());
        let one_time = upload
            .one_time_keys
            .iter()
            .map(|(id, key)| (id.to_string(), serde_json::to_value(key).unwrap()))
            .collect();
        let device = serde_json::to_value(upload.device_keys.as_ref().unwrap()).unwrap();
        let signing = bootstrap.upload_signing_keys_req;
        let mut query = json!({
            "device_keys": {HUMAN: {HUMAN_DEVICE: device}},
            "master_keys": {HUMAN: signing.master_key.unwrap()},
            "self_signing_keys": {HUMAN: signing.self_signing_key.unwrap()},
            "user_signing_keys": {HUMAN: signing.user_signing_key.unwrap()},
            "failures": {}
        });
        merge_signatures(
            &mut query,
            &serde_json::to_value(bootstrap.upload_signatures_req.signed_keys).unwrap(),
        );
        let initial = query_response(&query);
        let (id, _) = human.query_keys_for_users([human.user_id()]);
        human.mark_request_as_sent(&id, &initial).await.unwrap();
        assert!(
            human
                .get_identity(human.user_id(), None)
                .await
                .unwrap()
                .unwrap()
                .is_verified()
        );
        Self {
            sender: sender.try_into().unwrap(),
            device: sender_device.into(),
            query,
            writes: Vec::new(),
            claims: 0,
            shares: 0,
            events: Vec::new(),
            approval_markers: Vec::new(),
            human,
            one_time,
            sender_master: None,
            recipient_trusted: false,
            extra: BTreeMap::new(),
            extra_one_time: BTreeMap::new(),
            signed: BTreeSet::from([HUMAN_DEVICE.to_owned()]),
            parked: BTreeMap::new(),
            shared_devices: BTreeSet::new(),
        }
    }

    /// The owner logs in on another device (ADR-183 B). Its keys and one-time
    /// keys come from its own real machine; when `signed`, the owner's
    /// self-signing key signs it exactly as a verified login would be, and
    /// otherwise it stays an unverified device the pinned identity never
    /// signed. Nothing here touches the service SDK.
    pub async fn add_device(&mut self, id: &str, signed: bool) {
        let machine = OlmMachine::new(user_id!("@owner:example.test"), id.into()).await;
        let outgoing = machine.outgoing_requests().await.unwrap();
        let upload = outgoing
            .iter()
            .find_map(|r| match r.request() {
                AnyOutgoingRequest::KeysUpload(upload) => Some(upload.clone()),
                _ => None,
            })
            .expect("fresh device upload");
        let device = serde_json::to_value(upload.device_keys.as_ref().unwrap()).unwrap();
        let one_time = upload
            .one_time_keys
            .iter()
            .map(|(k, v)| (k.to_string(), serde_json::to_value(v).unwrap()))
            .collect::<BTreeMap<_, _>>();
        assert!(!one_time.is_empty());
        self.query["device_keys"][HUMAN][id] = device;
        self.extra_one_time.insert(id.to_owned(), one_time);
        self.extra.insert(id.to_owned(), machine);
        if signed {
            let response = query_response(&self.query);
            let (request_id, _) = self.human.query_keys_for_users([self.human.user_id()]);
            self.human
                .mark_request_as_sent(&request_id, &response)
                .await
                .unwrap();
            let device = self
                .human
                .get_device(self.human.user_id(), id.into(), None)
                .await
                .unwrap()
                .expect("owner sees its new device");
            let signature = device.verify().await.unwrap();
            merge_signatures(
                &mut self.query,
                &serde_json::to_value(signature.signed_keys).unwrap(),
            );
            self.signed.insert(id.to_owned());
        }
    }

    /// The owner removes (logs out) a device: it leaves `/keys/query`.
    pub fn park_device(&mut self, id: &str) {
        let entry = self.query["device_keys"][HUMAN]
            .as_object_mut()
            .unwrap()
            .remove(id)
            .expect("parked device exists");
        self.parked.insert(id.to_owned(), entry);
    }
    /// The parked device is listed again exactly as it was.
    pub fn restore_device(&mut self, id: &str) {
        let entry = self.parked.remove(id).expect("device was parked");
        self.query["device_keys"][HUMAN][id] = entry;
    }

    /// The owner resets their cross-signing identity: a new master key that
    /// the pinned anchor no longer names, properly self-signed and signing
    /// the owner's device.
    pub async fn reset_identity(&mut self) {
        let bootstrap = self.human.bootstrap_cross_signing(true).await.unwrap();
        let signing = bootstrap.upload_signing_keys_req;
        self.query["master_keys"][HUMAN] = json!(signing.master_key.unwrap());
        self.query["self_signing_keys"][HUMAN] = json!(signing.self_signing_key.unwrap());
        self.query["user_signing_keys"][HUMAN] = json!(signing.user_signing_key.unwrap());
        merge_signatures(
            &mut self.query,
            &serde_json::to_value(bootstrap.upload_signatures_req.signed_keys).unwrap(),
        );
        self.recipient_trusted = false;
    }

    pub fn anchor(&self) -> String {
        let keys = self.query["master_keys"][HUMAN]["keys"]
            .as_object()
            .unwrap();
        assert_eq!(keys.len(), 1);
        keys.values().next().unwrap().as_str().unwrap().into()
    }

    /// Another service account talks to this same independently owned human
    /// device. Split its unused original OTKs, never issue a key twice or copy
    /// a private service identity into either SDK under test.
    pub fn additional_sender(&mut self, sender: &str, device: &str) -> Self {
        assert!(self.one_time.len() >= 2);
        let split = self
            .one_time
            .keys()
            .nth(self.one_time.len() / 2)
            .unwrap()
            .clone();
        let one_time = self.one_time.split_off(&split);
        let mut query = json!({"device_keys":{}, "master_keys":{}, "self_signing_keys":{}, "user_signing_keys":{}, "failures":{}});
        for table in [
            "device_keys",
            "master_keys",
            "self_signing_keys",
            "user_signing_keys",
        ] {
            query[table][HUMAN] = self.query[table][HUMAN].clone();
        }
        Self {
            sender: sender.try_into().unwrap(),
            device: device.into(),
            query,
            writes: Vec::new(),
            claims: 0,
            shares: 0,
            events: Vec::new(),
            approval_markers: Vec::new(),
            human: self.human.clone(),
            one_time,
            sender_master: None,
            recipient_trusted: false,
            extra: BTreeMap::new(),
            extra_one_time: BTreeMap::new(),
            signed: BTreeSet::from([HUMAN_DEVICE.to_owned()]),
            parked: BTreeMap::new(),
            shared_devices: BTreeSet::new(),
        }
    }

    /// Actual request-derived server state. The TLS test harness must check its
    /// original ordinary bearer credential before calling this protocol fixture.
    /// Unknown routes are returned to that harness, never silently acknowledged.
    pub async fn protocol(
        &mut self,
        method: &str,
        target: &str,
        body: &Value,
    ) -> Option<(u16, Value)> {
        const MARKER: &str = "com.agentchat.approval.room.v2";
        if method == "PUT" {
            if let Some(room) = target.strip_prefix("/_matrix/client/v3/rooms/")
                .and_then(|tail| tail.strip_suffix(&format!("/state/{MARKER}/"))) {
                assert_eq!(body["publisher_mxid"], self.sender.as_str());
                assert_eq!(body["owner_mxid"], HUMAN);
                assert_eq!(body["version"], 2);
                let id = format!("$approval_marker_{}", self.approval_markers.len() + 1);
                self.approval_markers.push(json!({"type":MARKER,"state_key":"","event_id":id,
                    "room_id":room,"sender":self.sender.as_str(),"content":body}));
                return Some((200, json!({"event_id":id})));
            }
        }
        if method == "GET" && target.contains("/event/") {
            if let Some(event) = self.approval_markers.iter().find(|event| {
                let id = event["event_id"].as_str().unwrap();
                target.ends_with(&format!("/event/{id}"))
                    || target.ends_with(&format!("/event/%24{}", &id[1..]))
            }) {
                return Some((200, event.clone()));
            }
        }
        if method == "GET" && target == "/_matrix/client/versions" {
            return Some((200, json!({"versions":["v1.11","v1.12"]})));
        }
        if method == "POST" && target == "/_matrix/client/v3/keys/query" {
            let requested = body["device_keys"].as_object().unwrap();
            assert!(!requested.is_empty() && requested.len() <= 2);
            let mut response = json!({
                "device_keys":{}, "master_keys":{}, "self_signing_keys":{},
                "user_signing_keys":{}, "failures":{}
            });
            for (user, selection) in requested {
                assert!([self.sender.as_str(), HUMAN].contains(&user.as_str()));
                assert!(
                    selection.as_array().unwrap().is_empty(),
                    "fixture expects all original devices"
                );
                response["device_keys"][user] = self.query["device_keys"]
                    .get(user)
                    .cloned()
                    .unwrap_or(json!({}));
                for field in ["master_keys", "self_signing_keys", "user_signing_keys"] {
                    // A real homeserver exposes user-signing keys only to their owner.
                    if field == "user_signing_keys" && user != self.sender.as_str() {
                        continue;
                    }
                    if let Some(value) = self.query[field].get(user) {
                        response[field][user] = value.clone();
                    }
                }
            }
            return Some((200, response));
        }
        if method != "POST" {
            return None;
        }
        match target {
            "/_matrix/client/v3/keys/upload" => {
                assert!(body.get("auth").is_none());
                assert_eq!(
                    self.writes.len(),
                    0,
                    "fresh device upload must be first and single"
                );
                let device = &body["device_keys"];
                assert_eq!(device["user_id"], self.sender.as_str());
                assert_eq!(device["device_id"], self.device);
                assert!(device["keys"].as_object().unwrap().len() >= 2);
                let otks = body["one_time_keys"].as_object().unwrap();
                assert!(!otks.is_empty());
                for (id, key) in otks {
                    assert!(id.starts_with("signed_curve25519:"));
                    assert!(
                        key["signatures"][self.sender.as_str()]
                            .as_object()
                            .is_some_and(|s| !s.is_empty())
                    );
                }
                self.query["device_keys"][self.sender.as_str()] =
                    json!({(self.device.as_str()): device});
                self.writes.push((target.into(), body.clone()));
                Some((
                    200,
                    json!({"one_time_key_counts":{"signed_curve25519":otks.len()}}),
                ))
            }
            "/_matrix/client/v3/keys/device_signing/upload" => {
                assert!(body.get("auth").is_none());
                assert_eq!(self.writes.len(), 1);
                let original = &body["master_key"];
                if let Some(existing) = self.query["master_keys"].get(self.sender.as_str())
                    && existing != original
                {
                    // Ordinary-client UIA race behavior; never accept a reset.
                    return Some((
                        401,
                        json!({"flows":[{"stages":["m.login.password"]}],"session":"fixture-uia"}),
                    ));
                }
                for (field, table) in [
                    ("master_key", "master_keys"),
                    ("self_signing_key", "self_signing_keys"),
                    ("user_signing_key", "user_signing_keys"),
                ] {
                    assert_eq!(body[field]["user_id"], self.sender.as_str());
                    assert_eq!(body[field]["keys"].as_object().unwrap().len(), 1);
                    self.query[table][self.sender.as_str()] = body[field].clone();
                }
                self.sender_master = Some(original.clone());
                self.writes.push((target.into(), body.clone()));
                Some((200, json!({})))
            }
            "/_matrix/client/v3/keys/signatures/upload" => {
                assert!(
                    (2..4).contains(&self.writes.len()),
                    "one own and one peer signature upload"
                );
                assert!(body.get("auth").is_none());
                merge_signatures(&mut self.query, body);
                self.writes.push((target.into(), body.clone()));
                Some((200, json!({"failures":{}})))
            }
            "/_matrix/client/v3/keys/claim" => {
                if self.claims == 0 {
                    assert_eq!(
                        self.writes.len(),
                        4,
                        "the first claim follows the key writes"
                    );
                }
                // One claim per device that lacks a session: the original
                // owner device on the first claim, and any device the owner
                // added since (ADR-183 B) on a later one. Every claimed
                // device is one the owner really has; each key is issued once.
                let requested = body["one_time_keys"].as_object().unwrap();
                assert_eq!(requested.keys().collect::<Vec<_>>(), vec![HUMAN]);
                let mut keys = serde_json::Map::new();
                for (device, algorithm) in requested[HUMAN].as_object().unwrap() {
                    assert_eq!(algorithm, "signed_curve25519");
                    assert!(
                        self.query["device_keys"][HUMAN].get(device).is_some(),
                        "claim for a device the owner does not have: {device}"
                    );
                    let pool = if device == HUMAN_DEVICE {
                        &mut self.one_time
                    } else {
                        self.extra_one_time.get_mut(device).unwrap()
                    };
                    let (id, key) = pool.pop_first().unwrap();
                    keys.insert(device.clone(), json!({ id: key }));
                }
                assert!(!keys.is_empty());
                self.claims += 1;
                self.writes.push((target.into(), body.clone()));
                Some((200, json!({"one_time_keys":{HUMAN:keys},"failures":{}})))
            }
            _ => None,
        }
    }

    /// Explicit recipient-side fixture operator provisioning. The original
    /// service master was captured from its authenticated actual signing upload.
    /// The recipient signs that exact public identity locally; this does not add
    /// a signature to server responses or modify any service trust/session state.
    async fn trust_original_sender(&mut self) {
        if self.recipient_trusted {
            return;
        }
        let master = self
            .sender_master
            .as_ref()
            .expect("no original service key upload");
        assert_eq!(
            master["keys"],
            self.query["master_keys"][self.sender.as_str()]["keys"]
        );
        let mut local = self.query.clone();
        let response = query_response(&local);
        let (id, _) = self
            .human
            .query_keys_for_users([self.human.user_id(), self.sender.as_ref()]);
        self.human
            .mark_request_as_sent(&id, &response)
            .await
            .unwrap();
        let UserIdentity::Other(sender) = self
            .human
            .get_identity(self.sender.as_ref(), None)
            .await
            .unwrap()
            .unwrap()
        else {
            panic!("independent recipient did not accept service identity")
        };
        let signature = sender.verify().await.unwrap();
        merge_signatures(
            &mut local,
            &serde_json::to_value(signature.signed_keys).unwrap(),
        );
        let response = query_response(&local);
        let (id, _) = self
            .human
            .query_keys_for_users([self.human.user_id(), self.sender.as_ref()]);
        self.human
            .mark_request_as_sent(&id, &response)
            .await
            .unwrap();
        assert!(
            self.human
                .get_identity(self.sender.as_ref(), None)
                .await
                .unwrap()
                .unwrap()
                .is_verified()
        );
        self.recipient_trusted = true;
    }

    /// Independent owner initiates a new real Olm session using one actual
    /// service-uploaded signed OTK, then sends an encrypted Megolm room key.
    pub async fn inbound_room_key(&mut self, room: &RoomId) -> Value {
        self.trust_original_sender().await;
        // A previously received card key share can already have established
        // this real Olm session. Never invent a second claim in that case.
        if let Some((id, _)) = self
            .human
            .get_missing_sessions([self.sender.as_ref()].into_iter())
            .await
            .unwrap()
        {
            let upload = &self.writes[0].1;
            let (key_id, key) = upload["one_time_keys"]
                .as_object()
                .unwrap()
                .iter()
                .next()
                .unwrap();
            let response = json!({"one_time_keys":{(self.sender.as_str()):{(self.device.as_str()):{key_id:key}}},"failures":{}});
            let response = ruma::api::client::keys::claim_keys::v3::Response::new(
                serde_json::from_value(response["one_time_keys"].clone()).unwrap(),
            );
            self.human
                .mark_request_as_sent(&id, &response)
                .await
                .unwrap();
        }
        let shares = self
            .human
            .share_room_key(
                room,
                [self.sender.as_ref()].into_iter(),
                matrix_sdk_crypto::EncryptionSettings {
                    sharing_strategy: matrix_sdk_crypto::CollectStrategy::OnlyTrustedDevices,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(shares.len(), 1);
        let messages = serde_json::to_value(&shares[0].messages).unwrap();
        json!({"next_batch":"enrollment-inbound-olm","rooms":{},"to_device":{"events":[{
            "type":"m.room.encrypted","sender":HUMAN,"content":messages[self.sender.as_str()][&self.device]
        }]}})
    }

    /// Encrypt from the independent owner's actual outbound Megolm session.
    /// The caller must first deliver inbound_room_key to the service SDK.
    pub async fn owner_event(&self, room: &RoomId, content: Value) -> Value {
        let raw = Raw::from_json_string(content.to_string()).unwrap();
        let encrypted = self
            .human
            .encrypt_room_event_raw(room, "m.room.message", &raw)
            .await
            .unwrap();
        json!({"type":"m.room.encrypted","sender":HUMAN,"event_id":"$owner_verdict",
            "origin_server_ts":1,"content":encrypted.content})
    }

    pub async fn share(&mut self, value: Value) {
        self.trust_original_sender().await;
        assert_eq!(value["messages"].as_object().unwrap().len(), 1);
        let devices = value["messages"][HUMAN].as_object().unwrap();
        assert!(!devices.is_empty());
        for (device, content) in devices {
            assert!(content.is_object());
            // ADR-183 B: a room key reaches only devices the owner signed.
            assert!(
                self.signed.contains(device),
                "room key shared with a device the owner never signed: {device}"
            );
            let raw = Raw::from_json_string(
                json!({"type":"m.room.encrypted","sender":self.sender.as_str(),"content":content})
                    .to_string(),
            )
            .unwrap();
            let machine = if device == HUMAN_DEVICE {
                &self.human
            } else {
                &self.extra[device]
            };
            let (_, keys) = machine
                .receive_sync_changes(
                    EncryptionSyncChanges {
                        to_device_events: vec![raw],
                        changed_devices: &Default::default(),
                        one_time_keys_counts: &Default::default(),
                        unused_fallback_keys: None,
                        next_batch_token: None,
                    },
                    &DecryptionSettings {
                        sender_device_trust_requirement: TrustRequirement::CrossSigned,
                    },
                )
                .await
                .unwrap();
            assert_eq!(
                keys.len(),
                1,
                "actual signed Olm claim must deliver a decryptable Megolm key to {device}"
            );
            self.shared_devices.insert(device.clone());
        }
        self.shares += 1;
    }

    pub async fn decrypt(&mut self, value: Value, room: &RoomId) -> Value {
        assert!(self.recipient_trusted && self.shares > 0);
        let raw = Raw::from_json_string(
            json!({
                "type":"m.room.encrypted", "sender":self.sender.as_str(), "event_id":"$file_received",
                "origin_server_ts":1, "content":value
            })
            .to_string(),
        )
        .unwrap();
        let plain = self
            .human
            .decrypt_room_event(
                &raw,
                room,
                &DecryptionSettings {
                    sender_device_trust_requirement: TrustRequirement::CrossSigned,
                },
            )
            .await
            .unwrap();
        let event = serde_json::from_str(plain.event.json().get()).unwrap();
        self.events.push(event);
        self.events.last().unwrap().clone()
    }

    /// The last room event, decrypted on one of the owner's other devices:
    /// the proof that the card was encrypted to that device too. That device
    /// has not verified the service identity itself, so the sender-trust
    /// requirement is not applied here — only the key's presence is proven.
    pub async fn decrypt_on(&self, device: &str, value: Value, room: &RoomId) -> Value {
        let raw = Raw::from_json_string(
            json!({
                "type":"m.room.encrypted", "sender":self.sender.as_str(), "event_id":"$on_device",
                "origin_server_ts":1, "content":value
            })
            .to_string(),
        )
        .unwrap();
        let plain = self.extra[device]
            .decrypt_room_event(
                &raw,
                room,
                &DecryptionSettings {
                    sender_device_trust_requirement: TrustRequirement::Untrusted,
                },
            )
            .await
            .unwrap();
        serde_json::from_str(plain.event.json().get()).unwrap()
    }
}

/// Apply server signature upload semantics while retaining the original signed
/// object. The fixture refuses a signature request that substitutes key fields.
fn merge_signatures(query: &mut Value, request: &Value) {
    for (user, objects) in request.as_object().unwrap() {
        for (id, signed) in objects.as_object().unwrap() {
            let mut candidates = Vec::new();
            if query["device_keys"][user].get(id).is_some() {
                candidates.push("device_keys");
            }
            for table in ["master_keys", "self_signing_keys", "user_signing_keys"] {
                if query[table][user]["keys"]
                    .as_object()
                    .is_some_and(|keys| keys.values().any(|key| key.as_str() == Some(id.as_str())))
                {
                    candidates.push(table);
                }
            }
            assert_eq!(candidates.len(), 1, "unknown or colliding signed key ID");
            let current = if candidates[0] == "device_keys" {
                &mut query["device_keys"][user][id]
            } else {
                &mut query[candidates[0]][user]
            };
            for (field, value) in signed.as_object().unwrap() {
                if field != "signatures" && field != "unsigned" {
                    assert_eq!(
                        current.get(field),
                        Some(value),
                        "signature upload changed {field}"
                    );
                }
            }
            for (signer, signatures) in signed["signatures"].as_object().unwrap() {
                if current["signatures"].get(signer).is_none() {
                    current["signatures"][signer] = json!({});
                }
                for (id, value) in signatures.as_object().unwrap() {
                    current["signatures"][signer][id] = value.clone();
                }
            }
        }
    }
}

fn query_response(value: &Value) -> get_keys::v3::Response {
    let mut response = get_keys::v3::Response::new();
    response.device_keys = serde_json::from_value(value["device_keys"].clone()).unwrap();
    response.master_keys = serde_json::from_value(value["master_keys"].clone()).unwrap();
    response.self_signing_keys =
        serde_json::from_value(value["self_signing_keys"].clone()).unwrap();
    response.user_signing_keys =
        serde_json::from_value(value["user_signing_keys"].clone()).unwrap();
    response
}
