//! "N updates" on a run card: which thread events count, and the open state.
use super::*;
use serde_json::json;

const AGENT: &str = "@anna-claude:test";
fn message(body: &str) -> Value {
    json!({"type":"m.room.message","sender":AGENT,"content":{"msgtype":"m.text","body":body}})
}

#[test]
fn updates_read_oldest_first_one_short_line_each() {
    let newest_first = vec![
        message("running cargo test\nmore output"),
        json!({"type":"m.room.message","content":{"body":"* edited","m.relates_to":{"rel_type":"m.replace","event_id":"$a"}}}),
        json!({"type":"m.reaction","content":{}}),
        message(&"x".repeat(LINE_CHARS + 3)),
        message("reading the issue list"),
        json!({"type":"m.room.message","sender":"@betty:test","content":{"msgtype":"m.text","body":"> <@anna-claude:test> q\n\nuse the staging repo"}}),
        json!({"type":"m.room.message","sender":"@other-codex:test","content":{"msgtype":"m.text","body":"my own run"}}),
        json!({"type":"m.room.message","sender":AGENT,"content":{"msgtype":"im.hagency.agent_run","body":"⏳"}}),
    ];
    let agents = |s: &str| s.ends_with("-codex:test") || s == AGENT;
    let shown = lines(&newest_first, AGENT, agents);
    assert_eq!(shown.len(), 4);
    assert_eq!(shown[0], "• betty: use the staging repo", "a teammate's reply in the thread stays reachable");
    assert_eq!(shown[1], "• reading the issue list");
    assert!(shown[2].ends_with('…') && shown[2].chars().count() == LINE_CHARS + 3);
    assert_eq!(shown[3], "• running cargo test");
}

#[test]
fn the_open_state_has_words_for_every_case() {
    assert_eq!(text(&Updates::Loaded(vec!["• a".into(), "• b".into()])), "• a\n• b");
    for key in ["Loading updates…", "Could not load the updates. Close and open them to try again.", "No updates yet.", "Hide updates"] {
        assert_ne!(crate::i18n::text_for(crate::i18n::Language::Chinese, key), key, "{key} has no Chinese text");
    }
}

fn info(sender: &str, state: matrix_sdk::deserialized_responses::VerificationState) -> EncryptionInfo {
    use matrix_sdk::deserialized_responses::AlgorithmInfo;
    EncryptionInfo {
        sender: matrix_sdk::ruma::OwnedUserId::try_from(sender).unwrap(),
        sender_device: None,
        forwarder: None,
        algorithm_info: AlgorithmInfo::MegolmV1AesSha2 { curve25519_key: "k".into(), sender_claimed_keys: Default::default(), session_id: Some("s".into()) },
        verification_state: state,
    }
}

/// Integration GPT review: a plaintext thread message claiming the
/// Agent's ID in an encrypted room showed as the run's progress.
#[test]
fn only_authentic_events_count_as_updates_in_an_encrypted_room() {
    use matrix_sdk::deserialized_responses::{DeviceLinkProblem, VerificationLevel, VerificationState};
    let forged = message("deploying to production");
    // Encrypted (or unknown) room: a plaintext event never counts.
    assert!(!authentic_update(&forged, None, false));
    // Decrypted from the Agent's own known device: counts.
    assert!(authentic_update(&forged, Some(&info(AGENT, VerificationState::Verified)), false));
    assert!(authentic_update(&forged, Some(&info(AGENT, VerificationState::Unverified(VerificationLevel::UnsignedDevice))), false));
    // Decrypted, but from another account or an unknown device: refused.
    assert!(!authentic_update(&forged, Some(&info("@mallory:test", VerificationState::Verified)), false));
    let missing = VerificationState::Unverified(VerificationLevel::None(DeviceLinkProblem::MissingDevice));
    assert!(!authentic_update(&forged, Some(&info(AGENT, missing)), false));
    assert!(!authentic_update(&forged, Some(&info(AGENT, VerificationState::Unverified(VerificationLevel::MismatchedSender))), false));
    // A room known to be unencrypted keeps its plaintext updates.
    assert!(authentic_update(&forged, None, true));
    assert!(!authentic_update(&json!({"type":"m.room.encrypted","sender":AGENT}), None, true));
    assert!(!authentic_update(&json!({"type":"m.room.message"}), None, true), "no sender");
}

/// A card closed while its updates load stays closed; a superseded load
/// never replaces a newer one (GPT review).
#[test]
fn closing_or_reloading_drops_the_older_answer() {
    let key = key("!r:test", "$closing", AGENT);
    let begin = |k: &Key| { let mut s = store(); s.next += 1; let t = s.next; s.latest.insert(k.clone(), t); t };
    store().open.insert(key.clone(), Updates::Loading);
    let first = begin(&key);
    store().open.remove(&key);
    finish(&key, first, 2, Updates::Loaded(vec!["• late".into()]));
    assert_eq!(shown(&key.1, &key.2, AGENT), None);
    store().open.insert(key.clone(), Updates::Loading);
    let old = begin(&key);
    let new = begin(&key);
    finish(&key, new, 3, Updates::Loaded(vec!["• now".into()]));
    finish(&key, old, 2, Updates::Failed);
    assert_eq!(shown(&key.1, &key.2, AGENT), Some(Updates::Loaded(vec!["• now".into()])));
    assert_eq!(shown(&key.1, &key.2, "@other:test"), None, "keyed by Agent");
}
