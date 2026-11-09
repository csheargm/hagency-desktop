use super::*;

#[test]
fn postpaid_task_can_overrun_and_continue_without_holding_an_estimate() {
    let root = tempfile::tempdir().unwrap();
    let mut ledger = setup(&root.path().join("ledger.db"));
    assert_eq!(ledger.begin_postpaid(&scope(), "first", "task", 10).unwrap(), Reservation::New);
    assert_eq!(ledger.account(&scope(), Layer::Room, Period::UtcDay, 10).unwrap(), (0, 0));
    assert_eq!(ledger.begin_postpaid(&scope(), "first", "task", 10).unwrap(), Reservation::AlreadyPending);
    assert!(matches!(ledger.reserve(&scope(), "first", "task", 100, 10), Err(Error::Conflict)));
    ledger.settle(&scope(), "first", &usage(150)).unwrap();
    ledger.settle(&scope(), "first", &usage(150)).unwrap();
    assert_eq!(ledger.account(&scope(), Layer::Room, Period::UtcDay, 10).unwrap(), (150, 0));
    // The same admitted task may have more native model calls after overshoot.
    assert_eq!(ledger.begin_postpaid(&scope(), "next-call", "task", 10).unwrap(), Reservation::New);
    ledger.settle(&scope(), "next-call", &usage(30)).unwrap();
    assert!(matches!(ledger.begin_postpaid(&scope(), "new-task", "new-task", 10), Err(Error::Budget)));
    assert_eq!(ledger.account(&scope(), Layer::Room, Period::UtcDay, 10).unwrap(), (180, 0));
    assert!(matches!(ledger.settle(&scope(), "first", &usage(151)), Err(Error::Conflict)));
}

#[test]
fn postpaid_missing_usage_survives_restart_and_does_not_lock_other_work() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("ledger.db");
    let mut ledger = setup(&path);
    ledger.begin_postpaid(&scope(), "unknown", "task", 10).unwrap();
    ledger.settle(&scope(), "unknown", &unreported_usage()).unwrap();
    drop(ledger);
    let mut ledger = Ledger::open(path, OWNER).unwrap();
    let outstanding = ledger.outstanding_calls(OWNER).unwrap();
    assert_eq!(outstanding.len(), 1);
    assert_eq!((outstanding[0].reserved, outstanding[0].state.as_str()), (0, "unknown"));
    assert!(matches!(ledger.begin_postpaid(&scope(), "unknown", "task", 10), Err(Error::Unknown)));
    ledger.begin_postpaid(&scope(), "other", "other-task", 10).unwrap();
    ledger.settle(&scope(), "other", &usage(20)).unwrap();
    // A late real receipt is charged once, including an overrun.
    ledger.settle(&scope(), "unknown", &usage(130)).unwrap();
    ledger.settle(&scope(), "unknown", &usage(130)).unwrap();
    assert_eq!(ledger.account(&scope(), Layer::Room, Period::UtcDay, 10).unwrap(), (150, 0));
    assert!(ledger.outstanding_calls(OWNER).unwrap().is_empty());
}

#[test]
fn postpaid_keeps_old_unknown_records_and_still_enforces_request_identity() {
    let root = tempfile::tempdir().unwrap();
    let mut ledger = setup(&root.path().join("ledger.db"));
    ledger.reserve(&scope(), "legacy", "legacy-task", 100, 10).unwrap();
    ledger.mark_unknown(&scope(), "legacy").unwrap();
    ledger.begin_postpaid(&scope(), "new", "new-task", 10).unwrap();
    assert_eq!(ledger.account(&scope(), Layer::Room, Period::UtcDay, 10).unwrap(), (0, 100));
    assert!(matches!(ledger.begin_postpaid(&scope(), "legacy", "legacy-task", 10), Err(Error::Conflict)));
    let mut foreign = scope(); foreign.requester = "@mallory:example.org".into();
    assert!(matches!(ledger.begin_postpaid(&foreign, "new", "new-task", 10), Err(Error::Conflict)));
    let mut denied = config(100, Period::UtcDay); denied.requests = RequestPolicy::Deny;
    ledger.set_policy(OWNER, &scope(), Layer::Room, 1, &denied).unwrap();
    assert!(matches!(ledger.begin_postpaid(&scope(), "follow-up", "new-task", 10), Err(Error::Denied)));
    assert!(matches!(ledger.begin_postpaid(&scope(), "another", "another-task", 10), Err(Error::Denied)));
    // No old uncertain usage was forgiven or turned into a zero-usage receipt.
    let legacy = ledger.outstanding_calls(OWNER).unwrap().into_iter().find(|c| c.id == "legacy").unwrap();
    assert_eq!((legacy.reserved, legacy.state.as_str()), (100, "unknown"));
}
