use hagency_core::project::Resource;
use hagency_store::{DomainRepository, DomainStore, Error, management::*};
use serde_json::{Value, json};
use std::{
    future::Future,
    path::Path,
    time::{Duration, Instant},
};

fn context(account: &str, epoch: u64) -> HostContext {
    HostContext::new(account.into(), epoch, "a".repeat(64), 1, "device".into(), 1).unwrap()
}

fn resource(preset: &str, ceiling: Value) -> Resource {
    serde_json::from_value(json!({"presetId":preset,"seatId":"private_seat_reference",
        "framework":"codex","model":"gpt-5.6-sol","reasoning":"high",
        "provider":"private_provider_reference","ceiling":ceiling,"published":false}))
    .unwrap()
}
fn query(r: &Resource) -> ResourceDetailQuery {
    ResourceDetailQuery { id: r.id() }
}
fn bytes(path: &Path) -> (Vec<u8>, Option<Vec<u8>>) {
    (
        std::fs::read(path.join("domain.sqlite3")).unwrap(),
        std::fs::read(path.join("domain.sqlite3-wal")).ok(),
    )
}









#[test]
fn resource_detail_query_is_closed_and_bounded_before_host_initialization() {
    let valid = json!({"id":"resource_1"});
    serde_json::from_value::<ResourceDetailQuery>(valid.clone())
        .unwrap()
        .validate()
        .unwrap();
    for field in [
        "owner",
        "device",
        "path",
        "url",
        "provider",
        "canEdit",
        "create",
        "commandId",
    ] {
        let mut injected = valid.clone();
        injected[field] = json!("forged");
        assert!(serde_json::from_value::<ResourceDetailQuery>(injected).is_err());
    }
    for id in ["".into(), "x".repeat(129), "../resource".into()] {
        assert!(ResourceDetailQuery { id }.validate().is_err());
    }
}
