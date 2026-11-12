use hagency_core::project::Resource;
use hagency_store::{DomainRepository, Error, management::*};
use serde_json::json;

fn seed(db: &mut DomainRepository, id: &str, published: bool) -> Resource {
    let r: Resource = serde_json::from_value(json!({
        "presetId":id,"seatId":"local_account","framework":"codex",
        "model":"gpt-5.6-sol","reasoning":"medium","published":published,
        "ceiling":{"tokens":5000,"period":"monthly"}
    }))
    .unwrap();
    db.put_resource(&r).unwrap();
    r
}
#[test]
fn native_management_requests_cannot_supply_authority_or_arbitrary_paths() {
    assert!(
        serde_json::from_value::<ResourceQuery>(json!({"account":"@other:example.test"})).is_err()
    );
    assert!(
        HostContext::new(
            "not a Matrix user".into(),
            1,
            "a".repeat(64),
            1,
            "device".into(),
            1
        )
        .is_err()
    );
}

#[test]
fn managed_ledger_rejects_another_profile_and_populated_unscoped_import() {
    let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let managed = root.path().join("managed");
    let first = "a".repeat(64);
    let mut db = DomainRepository::open_managed(&managed, &first).unwrap();
    let resource = seed(&mut db, "retained", false);
    drop(db);
    assert!(matches!(
        DomainRepository::open_managed(&managed, &"b".repeat(64)),
        Err(Error::LocalAuthority)
    ));
    let db = DomainRepository::open_managed(&managed, &first).unwrap();
    assert_eq!(
        db.resource_configuration(&resource.id()).unwrap().model,
        resource.model
    );
    drop(db);
    let legacy = root.path().join("unscoped");
    let mut db = DomainRepository::open(&legacy).unwrap();
    seed(&mut db, "legacy", false);
    drop(db);
    assert!(matches!(
        DomainRepository::open_managed(&legacy, &first),
        Err(Error::LocalAuthority)
    ));
    assert_eq!(
        DomainRepository::open(&legacy)
            .unwrap()
            .resource_configurations("", 16)
            .unwrap()
            .len(),
        1
    );
}



#[test]
fn concurrent_private_directory_creation_remains_checked() {
    let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    for iteration in 0..16 {
        let target = root.path().join(format!("parallel_{iteration}"));
        let barrier = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        hagency_store::private::directory(&target)
                    })
                })
                .collect();
            for handle in handles {
                handle.join().unwrap().unwrap();
            }
        });
        hagency_store::private::directory(&target).unwrap();
    }
    let file = root.path().join("not_directory");
    std::fs::write(&file, b"unchanged").unwrap();
    assert!(hagency_store::private::directory(&file).is_err());
    #[cfg(unix)]
    {
        let link = root.path().join("linked");
        std::os::unix::fs::symlink(root.path(), &link).unwrap();
        assert!(hagency_store::private::directory(&link).is_err());
    }
}
